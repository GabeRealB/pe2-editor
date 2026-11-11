//! The log: every operation in transit, with its bytes, in the order it was made. Segment files of one size in the
//! library's folder (`log/<number>.seg`), each record where its position says - a position is a count of bytes from
//! the log's beginning, and so the sequence number of everything.
//!
//! Any process appends, one at a time: under a lock of its own threads and a lock the system holds on the header's
//! file - let go of by the system where its holder dies - a record is written with a write call, and only then is
//! the tail in the shared header moved past it. So a reader never sees part of a record, a writer that dies leaves no
//! hole, and a full disk is an error to who wrote, where a write through a mapping would end the process. Reading is
//! from mappings of the segments, with no call to the system.
//!
//! A record says its own position, so what a segment held before - one is removed once all it holds is in the
//! project, on disk (`Header::durable`), and its number never comes again - cannot be taken for a record. Where the
//! log ends is found, by the first process to open it, by checking records from that position on, never taken from
//! the header: after a power loss it ends where the last whole record does.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::Header;

/// The folder of the segments, in the library's folder.
pub const DIR: &str = "log";
/// The file each process holds a shared lock on while it has the log open: who finds nobody has, checks the log.
const ATTACHED: &str = "attached";

/// A record's head: its body's length, its kind, its own position, a checksum of all three and the body.
const HEAD: u64 = 24;
const HEAD_BYTES: usize = 24;
/// The kind of what is no record: a segment's untouched rest reads as zeros.
const NOTHING: u8 = 0;
/// The kind that says the rest of its segment holds nothing: the next record is at the next segment's start.
const SKIP: u8 = 255;

/// How a log is laid out and how much it may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The size of a segment's file: no record is larger, or lies across two.
    pub segment: u64,
    /// How much may wait in the log for the project to have it on disk: past it, appending is refused until a
    /// checkpoint lets go of what is before it.
    pub budget: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config { segment: 64 << 20, budget: 4 << 30 }
    }
}

/// The log of the project whose library folder this is given.
pub struct Log {
    dir: PathBuf,
    header: Header,
    config: Config,
    /// One of this process's threads appends at a time; with it, the segments it has open to write.
    appending: Mutex<HashMap<u64, File>>,
    /// The segments mapped to read.
    maps: RwLock<HashMap<u64, Arc<memmap2::Mmap>>>,
    /// How far this process has flushed the log to disk.
    flushed: Mutex<u64>,
    /// Held shared for as long as the log is open here.
    attached: File,
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "log in {} ({:?})", self.dir.display(), self.header)
    }
}

/// Bytes of the log, read where they are mapped.
#[derive(Clone)]
pub struct Bytes {
    map: Arc<memmap2::Mmap>,
    start: usize,
    len: usize,
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.map[self.start..self.start + self.len]
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} bytes", self.len)
    }
}

/// A record of the log.
#[derive(Clone, Debug)]
pub struct Record {
    pub kind: u8,
    /// Where it is: its sequence number.
    pub position: u64,
    /// Where its body is: a byte of it is at this plus its place in the body.
    pub body_at: u64,
    /// Where the record after it is, or will be.
    pub next: u64,
    pub body: Bytes,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn padded(len: u64) -> u64 {
    len.div_ceil(8) * 8
}

fn head(kind: u8, position: u64, body: &[&[u8]]) -> [u8; HEAD_BYTES] {
    let len: usize = body.iter().map(|part| part.len()).sum();
    let mut sum = crc32fast::Hasher::new();
    sum.update(&[kind]);
    sum.update(&position.to_le_bytes());
    for part in body {
        sum.update(part);
    }
    let mut out = [0u8; HEAD_BYTES];
    out[..4].copy_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
    out[4] = kind;
    out[8..16].copy_from_slice(&position.to_le_bytes());
    out[16..20].copy_from_slice(&sum.finalize().to_le_bytes());
    out
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], at: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, at)
}

#[cfg(windows)]
fn write_at(file: &File, mut bytes: &[u8], mut at: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let wrote = std::os::windows::fs::FileExt::seek_write(file, bytes, at)?;
        if wrote == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[wrote..];
        at += wrote as u64;
    }
    Ok(())
}

impl Log {
    /// Open the log kept in `dir`, made where there is none. The first process to have it open checks it: where it
    /// ends is where the last whole record does.
    ///
    /// # Errors
    /// Its folder, header or segments cannot be made, opened or read.
    pub fn open(dir: &Path, config: Config) -> io::Result<Log> {
        let header = Header::open(dir)?;
        std::fs::create_dir_all(dir.join(DIR))?;
        let attached =
            std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(ATTACHED))?;
        let log = Log {
            dir: dir.to_path_buf(),
            header,
            config,
            appending: Mutex::default(),
            maps: RwLock::default(),
            flushed: Mutex::new(0),
            attached,
        };
        // One process opens at a time: who has it alone checks it before another counts on its tail.
        log.header.file().lock()?;
        let checked = (|| {
            // Alone with it - asked a few times: a process starting another holds, for a moment, what its threads
            // have open, the locks on it with it.
            let alone = (0..5).any(|again| {
                if again > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                log.attached.try_lock().is_ok()
            });
            if alone {
                log.header.set_tail(log.end_of_whole()?);
                log.attached.unlock()?;
            }
            log.attached.lock_shared()
        })();
        log.header.file().unlock()?;
        checked?;
        *lock(&log.flushed) = log.header.durable();
        Ok(log)
    }

    /// The header this log shares with every process that has it open.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    fn segment_path(&self, number: u64) -> PathBuf {
        self.dir.join(DIR).join(format!("{number:016x}.seg"))
    }

    /// The segments there are, by number, lowest first.
    fn segments(&self) -> Vec<u64> {
        let names = std::fs::read_dir(self.dir.join(DIR)).into_iter().flatten().flatten().map(|entry| entry.file_name());
        let mut found: Vec<u64> = names
            .filter_map(|name| u64::from_str_radix(name.to_str()?.strip_suffix(".seg")?, 16).ok())
            .collect();
        found.sort_unstable();
        found
    }

    /// Where the last whole record ends, checking each from the position the project has on disk - or, where no
    /// record is there, from the first segment there is.
    fn end_of_whole(&self) -> io::Result<u64> {
        let size = self.config.segment;
        let durable = self.header.durable();
        let lowest = self.segments().first().map_or(durable, |number| number * size);
        let mut at = if durable >= lowest { durable } else { lowest };
        loop {
            match self.record_at(at, None, true) {
                Ok(Some(record)) => at = record.next,
                Ok(None) => return Ok(at),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(at),
                Err(e) => return Err(e),
            }
        }
    }

    /// Append a record of `kind` whose body is `body`'s parts one after another: where it is.
    ///
    /// # Errors
    /// The record is larger than a segment; the log holds as much as it may (`StorageFull`), until a checkpoint; or
    /// it cannot be written - a full disk among it - and then the log is as it was.
    pub fn append(&self, kind: u8, body: &[&[u8]]) -> io::Result<u64> {
        debug_assert!(kind != NOTHING && kind != SKIP, "kinds the log keeps for itself");
        let size = self.config.segment;
        let len: u64 = body.iter().map(|part| part.len() as u64).sum();
        let need = HEAD + padded(len);
        if need > size || len > u64::from(u32::MAX) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a record larger than a segment"));
        }
        let mut files = lock(&self.appending);
        self.header.file().lock()?;
        let appended = self.append_locked(&mut files, kind, body, need);
        self.header.file().unlock()?;
        appended
    }

    fn append_locked(&self, files: &mut HashMap<u64, File>, kind: u8, body: &[&[u8]], need: u64) -> io::Result<u64> {
        let size = self.config.segment;
        let mut tail = self.header.tail();
        // What does not fit the rest of a segment starts the next: the rest says so, where a head fits there.
        let left = size - tail % size;
        if need > left {
            if left >= HEAD {
                write_at(self.segment_file(files, tail / size)?, &head(SKIP, tail, &[]), tail % size)?;
            }
            tail += left;
        }
        if tail + need - self.header.durable().min(tail) > self.config.budget {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "the log holds as much as it may"));
        }
        let file = self.segment_file(files, tail / size)?;
        let at = tail % size;
        // One write where it is small; the head after the body where not, so that a head is never there without it.
        if need <= 64 << 10 {
            let mut whole = Vec::with_capacity(usize::try_from(need).unwrap_or(0));
            whole.extend_from_slice(&head(kind, tail, body));
            for part in body {
                whole.extend_from_slice(part);
            }
            write_at(file, &whole, at)?;
        } else {
            let mut body_at = at + HEAD;
            for part in body {
                write_at(file, part, body_at)?;
                body_at += part.len() as u64;
            }
            write_at(file, &head(kind, tail, body), at)?;
        }
        self.header.set_tail(tail + need);
        Ok(tail)
    }

    /// The file of a segment, open to write: made, of a segment's whole length, where it is not there.
    fn segment_file<'a>(&self, files: &'a mut HashMap<u64, File>, number: u64) -> io::Result<&'a File> {
        if !files.contains_key(&number) {
            let path = self.segment_path(number);
            let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
            if file.metadata()?.len() < self.config.segment {
                file.set_len(self.config.segment)?;
            }
            // Those long behind are let go of: a process appends to one or two at a time.
            files.retain(|open, _| open + 2 > number);
            files.insert(number, file);
        }
        Ok(&files[&number])
    }

    /// A segment mapped to read: whole, as its file is from the moment it is made.
    fn map(&self, number: u64) -> io::Result<Arc<memmap2::Mmap>> {
        if let Some(map) = self.maps.read().unwrap_or_else(PoisonError::into_inner).get(&number) {
            return Ok(map.clone());
        }
        let file = File::open(self.segment_path(number))?;
        if file.metadata()?.len() < self.config.segment {
            return Err(io::Error::new(io::ErrorKind::NotFound, "a segment not whole yet"));
        }
        // SAFETY: a segment's file is only ever written by appends past the tail, and read only before it: what is
        // read does not change under the mapping. It is never made shorter while it is there.
        let map = Arc::new(unsafe { memmap2::Mmap::map(&file)? });
        let mut maps = self.maps.write().unwrap_or_else(PoisonError::into_inner);
        Ok(maps.entry(number).or_insert(map).clone())
    }

    /// The record at a position; none where the log ends there. `checked`, its checksum is too - what the first
    /// process to open the log does, where every other counts on the tail.
    fn record_at(&self, mut position: u64, tail: Option<u64>, checked: bool) -> io::Result<Option<Record>> {
        let size = self.config.segment;
        loop {
            if tail.is_some_and(|tail| position >= tail) {
                return Ok(None);
            }
            let left = size - position % size;
            if left < HEAD {
                position += left;
                continue;
            }
            let map = self.map(position / size)?;
            let at = usize::try_from(position % size).unwrap_or(usize::MAX);
            let head = &map[at..at + HEAD_BYTES];
            let len = u64::from(u32::from_le_bytes(head[..4].try_into().unwrap_or_default()));
            let kind = head[4];
            let said = u64::from_le_bytes(head[8..16].try_into().unwrap_or_default());
            let sum = u32::from_le_bytes(head[16..20].try_into().unwrap_or_default());
            // What says another position than its own is what the segment held before, or nothing at all.
            if kind == NOTHING || said != position || HEAD + padded(len) > left {
                return Ok(None);
            }
            let body = Bytes { map: map.clone(), start: at + HEAD_BYTES, len: usize::try_from(len).unwrap_or(0) };
            if checked {
                let mut whole = crc32fast::Hasher::new();
                whole.update(&[kind]);
                whole.update(&position.to_le_bytes());
                whole.update(&body);
                if whole.finalize() != sum {
                    return Ok(None);
                }
            }
            if kind == SKIP {
                position += left;
                continue;
            }
            let next = position + HEAD + padded(len);
            return Ok(Some(Record { kind, position, body_at: position + HEAD, next, body }));
        }
    }

    /// The record at a position, or the first after it where that is the rest of a segment; none at the log's end.
    ///
    /// # Errors
    /// Its segment cannot be mapped: it was let go of, all it held being the project's on disk.
    pub fn read(&self, position: u64) -> io::Result<Option<Record>> {
        self.record_at(position, Some(self.header.tail()), false)
    }

    /// The records from a position to where the log ends now, in order.
    pub fn records(&self, from: u64) -> impl Iterator<Item = io::Result<Record>> + '_ {
        let mut at = Some(from);
        std::iter::from_fn(move || match self.read(at?) {
            Ok(Some(record)) => {
                at = Some(record.next);
                Some(Ok(record))
            }
            Ok(None) => None,
            Err(e) => {
                at = None;
                Some(Err(e))
            }
        })
    }

    /// Bytes of the log by where they are: of one record's body, as its `body_at` and a place in it say.
    ///
    /// # Errors
    /// They are not within one segment's records before the tail, or the segment is gone.
    pub fn bytes(&self, at: u64, len: usize) -> io::Result<Bytes> {
        let size = self.config.segment;
        let start = at % size;
        if start + len as u64 > size || at + len as u64 > self.header.tail() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bytes not within the log"));
        }
        Ok(Bytes { map: self.map(at / size)?, start: usize::try_from(start).unwrap_or(0), len })
    }

    /// Have the log on disk up to a position: what is before it survives a loss of power.
    ///
    /// # Errors
    /// A segment cannot be flushed.
    pub fn flush(&self, upto: u64) -> io::Result<()> {
        let size = self.config.segment;
        let mut flushed = lock(&self.flushed);
        if *flushed >= upto {
            return Ok(());
        }
        // Up to where the log ends now: whoever asks next for less has it already.
        let upto = self.header.tail().max(upto);
        for number in *flushed / size..=(upto.saturating_sub(1)) / size {
            // Open to write: a system may flush only what could have been written through the handle.
            match std::fs::OpenOptions::new().write(true).open(self.segment_path(number)) {
                Ok(file) => file.sync_data()?,
                // Let go of: all it held is the project's, on disk.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        *flushed = upto;
        Ok(())
    }

    /// Let go of the segments wholly before the position the project has on disk (`Header::durable`): how many went.
    /// One another process still has mapped may stay, on a system that keeps such a file: it goes the next time.
    pub fn let_go(&self) -> usize {
        let size = self.config.segment;
        let keep_from = self.header.durable() / size;
        let old: Vec<u64> = self.segments().into_iter().filter(|number| *number < keep_from).collect();
        lock(&self.appending).retain(|number, _| *number >= keep_from);
        self.maps.write().unwrap_or_else(PoisonError::into_inner).retain(|number, _| *number >= keep_from);
        old.into_iter().filter(|number| std::fs::remove_file(self.segment_path(*number)).is_ok()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, HEAD, Log};
    use std::path::PathBuf;

    const SMALL: Config = Config { segment: 4096, budget: 1 << 20 };

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pe2-sfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn what_is_appended_is_read_where_it_says_in_order() {
        let dir = folder("order");
        let log = Log::open(&dir, SMALL).unwrap();
        let first = log.append(1, &[b"one"]).unwrap();
        let second = log.append(2, &[b"two", b" parts"]).unwrap();
        assert_eq!((first, second), (0, HEAD + 8));
        let read: Vec<(u8, u64, Vec<u8>)> =
            log.records(0).map(|r| r.unwrap()).map(|r| (r.kind, r.position, r.body.to_vec())).collect();
        assert_eq!(read, [(1, first, b"one".to_vec()), (2, second, b"two parts".to_vec())]);
        // A record's bytes by where they are, as an index of a file's content keeps them.
        let record = log.read(second).unwrap().unwrap();
        assert_eq!(&*log.bytes(record.body_at + 4, 5).unwrap(), b"parts");
        assert!(log.read(record.next).unwrap().is_none(), "the log ends after it");
        assert!(log.bytes(record.next, 1).is_err(), "nothing is read past the tail");
        // On disk up to there; asked again, there is nothing to do.
        log.flush(record.next).unwrap();
        log.flush(second).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_record_that_does_not_fit_a_segments_rest_starts_the_next() {
        let dir = folder("segments");
        let log = Log::open(&dir, SMALL).unwrap();
        let body = [7u8; 1500];
        let at: Vec<u64> = (0..5).map(|_| log.append(3, &[&body]).unwrap()).collect();
        // Two fit a segment of 4096, the third starts the second segment.
        assert_eq!(at[2], 4096);
        assert_eq!(at[4], 8192);
        let read: Vec<u64> = log.records(0).map(|r| r.unwrap().position).collect();
        assert_eq!(read, at, "read across segments, their rests skipped");
        assert!(log.records(0).all(|r| *r.unwrap().body == body));
        assert!(log.append(3, &[&[0u8; 5000]]).is_err(), "none is larger than a segment");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn several_threads_append_and_none_is_lost_or_torn() {
        let dir = folder("threads");
        let log = std::sync::Arc::new(Log::open(&dir, Config { segment: 1 << 16, budget: 1 << 30 }).unwrap());
        let threads: Vec<_> = (0..8u8)
            .map(|thread| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for n in 0..500u32 {
                        let body = [thread; 40];
                        log.append(1, &[&n.to_le_bytes(), &body[..=usize::from(thread)]]).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let mut seen = vec![0u32; 8];
        for record in log.records(0) {
            let record = record.unwrap();
            let (n, rest) = record.body.split_at(4);
            let thread = rest[0];
            assert!(rest.len() == usize::from(thread) + 1 && rest.iter().all(|b| *b == thread), "a body whole");
            assert_eq!(u32::from_le_bytes(n.try_into().unwrap()), seen[usize::from(thread)], "each thread's in order");
            seen[usize::from(thread)] += 1;
        }
        assert_eq!(seen, vec![500; 8]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_log_ends_where_its_last_whole_record_does() {
        let dir = folder("torn");
        let log = Log::open(&dir, SMALL).unwrap();
        let first = log.append(1, &[b"whole"]).unwrap();
        let second = log.append(1, &[b"torn by a loss of power"]).unwrap();
        let after = log.read(second).unwrap().unwrap().next;
        drop(log);
        // The second record's body half written, and what a segment held before after it.
        let segment = dir.join("log").join(format!("{:016x}.seg", 0));
        let mut bytes = std::fs::read(&segment).unwrap();
        let at = usize::try_from(second + HEAD).unwrap();
        bytes[at + 3] ^= 0xff;
        let stale = super::head(1, 9999, &[b"old"]);
        let at = usize::try_from(after).unwrap();
        bytes[at..at + stale.len()].copy_from_slice(&stale);
        std::fs::write(&segment, bytes).unwrap();
        let log = Log::open(&dir, SMALL).unwrap();
        assert_eq!(log.header().tail(), second, "it ends before what is not whole");
        assert_eq!(log.records(0).map(|r| r.unwrap().position).collect::<Vec<_>>(), [first]);
        // And goes on from there.
        assert_eq!(log.append(1, &[b"again"]).unwrap(), second);
        assert_eq!(log.records(0).count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_full_log_refuses_until_what_it_holds_is_the_projects_on_disk() {
        let dir = folder("budget");
        let log = Log::open(&dir, Config { segment: 4096, budget: 8192 }).unwrap();
        let body = [1u8; 1000];
        let mut last = 0;
        let full = loop {
            match log.append(1, &[&body]) {
                Ok(at) => last = at,
                Err(e) => break e,
            }
        };
        assert_eq!(full.kind(), std::io::ErrorKind::StorageFull);
        // A checkpoint: the project has all of it on disk. The segments before go, and there is room again.
        let end = log.read(last).unwrap().unwrap().next;
        log.header().set_applied(end);
        log.header().set_durable(end);
        assert_eq!(log.let_go(), 2, "the two segments it filled, wholly before it");
        let again = log.append(1, &[&body]).unwrap();
        assert!(again >= end);
        assert_eq!(log.records(end).count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What the second process of `two_processes_append_to_one_log` does: run by that test alone.
    #[test]
    #[ignore = "run by two_processes_append_to_one_log, as its second process"]
    fn the_second_process() {
        let Ok(dir) = std::env::var("PE2_SFS_SECOND") else { return };
        let log = Log::open(std::path::Path::new(&dir), Config { segment: 1 << 16, budget: 1 << 30 }).unwrap();
        for n in 0..2000u32 {
            log.append(2, &[&n.to_le_bytes(), b" from the second process"]).unwrap();
        }
    }

    #[test]
    fn two_processes_append_to_one_log() {
        let dir = folder("processes");
        let log = Log::open(&dir, Config { segment: 1 << 16, budget: 1 << 30 }).unwrap();
        let mut second = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "log::tests::the_second_process"])
            .env("PE2_SFS_SECOND", &dir)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for n in 0..2000u32 {
            log.append(1, &[&n.to_le_bytes(), b" from the first"]).unwrap();
        }
        assert!(second.wait().unwrap().success());
        // Read here, what the other appended among it: each process's in its order, every body whole.
        let mut next = [0u32, 0u32];
        for record in log.records(0) {
            let record = record.unwrap();
            let (n, said) = record.body.split_at(4);
            let which = usize::from(record.kind - 1);
            assert_eq!(said, [&b" from the first"[..], b" from the second process"][which]);
            assert_eq!(u32::from_le_bytes(n.try_into().unwrap()), next[which]);
            next[which] += 1;
        }
        assert_eq!(next, [2000, 2000]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// How long an append takes, by the size of what is appended.
    #[test]
    #[ignore = "a measurement"]
    fn bench_append() {
        let dir = folder("bench");
        let log = Log::open(&dir, Config::default()).unwrap();
        for size in [64usize, 4096, 128 << 10] {
            let body = vec![5u8; size];
            let count = if size > 4096 { 2000 } else { 20000 };
            let mut took: Vec<std::time::Duration> = (0..count)
                .map(|_| {
                    let started = std::time::Instant::now();
                    log.append(1, &[&body]).unwrap();
                    started.elapsed()
                })
                .collect();
            took.sort_unstable();
            let at = |part: usize| took[took.len() * part / 100].as_secs_f64() * 1e6;
            println!("BENCH append of {size} bytes: median {:.1} us, 99th {:.1} us", at(50), at(99));
        }
        let started = std::time::Instant::now();
        let count = log.records(0).count();
        println!("BENCH {count} records read in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
