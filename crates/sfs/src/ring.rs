//! The page ring: what was read of files, kept for whoever reads it next - in whichever process. Slots of 4 KiB in
//! memory the project's processes share, each the bytes of one page of one version of one file (`Key`): what is
//! read is copied from a slot, with no call into the system. It holds bytes that were put together - a record's
//! text out of its record, a large file out of its chunks, a file in transit out of the log's ranges - and is the
//! only copy of nothing: a miss is answered by reading where the bytes live, and putting them here.
//!
//! No lock: a slot has a counter that is odd while it is written. A reader checks it before and after copying, and
//! takes any doubt for a miss; a writer claims a slot by one exchange of that counter, and gives up where another
//! has it. A key is looked for in four slots its hash names, and put in the one of them used longest ago. A version
//! that is no longer read is never found again, and goes as its slots are taken for others. A process that dies
//! writing leaves its slot claimed: a miss from then on, until the ring is made anew (`Ring::clear`, by whoever
//! has the space alone).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// How many bytes a slot holds.
pub const PAGE: usize = 4096;
/// How many slots a key may be in.
const WAYS: usize = 4;
/// What says of a slot: its counter, its key, how many bytes it holds and when it was last used.
const SAID: usize = 40;

const MAGIC: u64 = u64::from_le_bytes(*b"pe2ring\x01");

/// What a slot holds: one page of one version of one file, by the file's number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub number: u64,
    pub version: u64,
    /// Which page of the file: its offset divided by a page's length.
    pub page: u64,
}

impl Key {
    fn hash(&self) -> u64 {
        let mut hash = self.number.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        hash = (hash ^ self.version).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        hash = (hash ^ self.page).wrapping_mul(0x1656_67B1_9E37_79F9);
        hash ^ (hash >> 29)
    }
}

/// The ring, mapped here.
pub struct Ring {
    map: memmap2::MmapRaw,
    /// How many slots it has: a multiple of `WAYS`.
    slots: usize,
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a ring of {} pages", self.slots)
    }
}

/// How long a ring of this many slots is: a page that says what it is, what is said of each slot, then the slots'
/// pages.
fn len_of(slots: usize) -> usize {
    PAGE + (slots * SAID).next_multiple_of(PAGE) + slots * PAGE
}

fn slots_in(bytes: u64) -> usize {
    let slots = usize::try_from(bytes).unwrap_or(usize::MAX) / (PAGE + SAID);
    (slots / WAYS).max(1) * WAYS
}

impl Ring {
    /// A ring of this process's own, of about this many bytes.
    ///
    /// # Errors
    /// There is no memory for it.
    pub fn private(bytes: u64) -> io::Result<Ring> {
        let slots = slots_in(bytes);
        let map = memmap2::MmapOptions::new().len(len_of(slots)).map_anon()?;
        let ring = Ring { map: map.into(), slots };
        ring.word(0).store(MAGIC, Ordering::Release);
        ring.word(8).store(slots as u64, Ordering::Release);
        Ok(ring)
    }

    /// The ring in a file, of about this many bytes, made where there is none - or where the one there is of
    /// another length. The file is as long as the ring from the start, but takes memory - or disk - only for the
    /// slots that were written: it belongs where the system keeps memory that processes share, not on a disk.
    ///
    /// # Errors
    /// The file cannot be made or mapped.
    pub fn open(path: &Path, bytes: u64) -> io::Result<Ring> {
        let slots = slots_in(bytes);
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        if file.metadata()?.len() != len_of(slots) as u64 {
            // Another length: none of it is this ring. Emptied first, so that all of it reads as nothing.
            file.set_len(0)?;
            file.set_len(len_of(slots) as u64)?;
        }
        let map = memmap2::MmapOptions::new().len(len_of(slots)).map_raw(&file)?;
        let ring = Ring { map, slots };
        // Whoever comes first says what it is; the others find it said.
        let said = ring.word(0).compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire);
        if said.is_err_and(|other| other != MAGIC) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no ring of this format"));
        }
        let _ = ring.word(8).compare_exchange(0, slots as u64, Ordering::AcqRel, Ordering::Acquire);
        if ring.word(8).load(Ordering::Acquire) != slots as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a ring of another length"));
        }
        Ok(ring)
    }

    /// How many pages it holds at most.
    #[must_use]
    pub fn pages(&self) -> usize {
        self.slots
    }

    #[expect(clippy::cast_ptr_alignment, reason = "the mapping begins a page, and `at` is a multiple of eight")]
    fn word(&self, at: usize) -> &AtomicU64 {
        debug_assert!(at.is_multiple_of(8) && at + 8 <= self.map.len());
        // SAFETY: within the mapping, aligned to eight bytes, valid for as long as `self`; every process reads and
        // writes it as an atomic.
        unsafe { &*self.map.as_mut_ptr().add(at).cast::<AtomicU64>() }
    }

    /// What is said of a slot begins here.
    fn said(slot: usize) -> usize {
        PAGE + slot * SAID
    }

    #[expect(clippy::cast_ptr_alignment, reason = "what is said of a slot begins at a multiple of eight, this four after one")]
    fn used(&self, slot: usize) -> &AtomicU32 {
        // SAFETY: as `word`: within what is said of the slot, aligned to four bytes.
        unsafe { &*self.map.as_mut_ptr().add(Ring::said(slot) + 36).cast::<AtomicU32>() }
    }

    #[expect(clippy::cast_ptr_alignment, reason = "what is said of a slot begins at a multiple of eight, this at one")]
    fn held(&self, slot: usize) -> &AtomicU32 {
        // SAFETY: as `word`.
        unsafe { &*self.map.as_mut_ptr().add(Ring::said(slot) + 32).cast::<AtomicU32>() }
    }

    fn data(&self, slot: usize) -> *mut u8 {
        let first = PAGE + (self.slots * SAID).next_multiple_of(PAGE);
        // SAFETY: within the mapping: the slots' pages follow what is said of them.
        unsafe { self.map.as_mut_ptr().add(first + slot * PAGE) }
    }

    /// The slots a key may be in.
    fn ways(&self, key: &Key) -> std::ops::Range<usize> {
        let first = usize::try_from(key.hash() % (self.slots / WAYS) as u64).unwrap_or(0) * WAYS;
        first..first + WAYS
    }

    fn is(&self, slot: usize, key: &Key) -> bool {
        let said = Ring::said(slot);
        self.word(said + 8).load(Ordering::Acquire) == key.number
            && self.word(said + 16).load(Ordering::Acquire) == key.version
            && self.word(said + 24).load(Ordering::Acquire) == key.page
    }

    /// The moment, as slots count when they were last used: it goes on with every page put.
    fn now(&self) -> u32 {
        Ring::moment(self.word(16).load(Ordering::Relaxed))
    }

    /// A count of pages put, as a slot keeps it: its low half - what matters is how far two are apart.
    fn moment(put: u64) -> u32 {
        u32::try_from(put & u64::from(u32::MAX)).unwrap_or(0)
    }

    /// The bytes of a page, copied into `out` where the ring holds them: how many. None is a miss, whatever the
    /// reason - never held, taken for another, or being written this moment.
    #[must_use]
    pub fn get(&self, key: &Key, out: &mut [u8; PAGE]) -> Option<usize> {
        for slot in self.ways(key) {
            let counter = self.word(Ring::said(slot));
            let before = counter.load(Ordering::Acquire);
            if before == 0 || before % 2 == 1 || !self.is(slot, key) {
                continue;
            }
            let len = (self.held(slot).load(Ordering::Acquire) as usize).min(PAGE);
            // SAFETY: the slot's page, within the mapping; what a writer changes of it meanwhile is found by the
            // counter read after, and thrown away.
            unsafe { std::ptr::copy_nonoverlapping(self.data(slot), out.as_mut_ptr(), len) };
            if counter.load(Ordering::Acquire) != before {
                return None;
            }
            // Said used now, where that was not said lately - within fewer pages put than fill one slot of every
            // eight: a slot read often is not the one taken next, and is not written at every read.
            let now = self.now();
            if now.wrapping_sub(self.used(slot).load(Ordering::Relaxed)) as usize > self.slots / 8 {
                self.used(slot).store(now, Ordering::Relaxed);
            }
            return Some(len);
        }
        None
    }

    /// Keep the bytes of a page - a page's length at most, less for a file's last. Not kept where every slot it
    /// may be in is being written this moment: it is a cache.
    pub fn put(&self, key: &Key, bytes: &[u8]) {
        let bytes = &bytes[..bytes.len().min(PAGE)];
        let now = Ring::moment(self.word(16).fetch_add(1, Ordering::Relaxed));
        // The slot that has the key; else one never used; else the one used longest ago.
        let mut taken: Option<(usize, u64, u32)> = None;
        for slot in self.ways(key) {
            let counter = self.word(Ring::said(slot)).load(Ordering::Acquire);
            if counter % 2 == 1 {
                continue;
            }
            if counter != 0 && self.is(slot, key) {
                taken = Some((slot, counter, u32::MAX));
                break;
            }
            let age = if counter == 0 { u32::MAX - 1 } else { now.wrapping_sub(self.used(slot).load(Ordering::Relaxed)) };
            if taken.is_none_or(|(_, _, oldest)| age > oldest) {
                taken = Some((slot, counter, age));
            }
        }
        let Some((slot, counter, _)) = taken else { return };
        let said = Ring::said(slot);
        // Claimed by one exchange: whoever else has it, or took it since, keeps it.
        if self.word(said).compare_exchange(counter, counter + 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        self.word(said + 8).store(key.number, Ordering::Release);
        self.word(said + 16).store(key.version, Ordering::Release);
        self.word(said + 24).store(key.page, Ordering::Release);
        self.held(slot).store(u32::try_from(bytes.len()).unwrap_or(0), Ordering::Release);
        self.used(slot).store(now, Ordering::Relaxed);
        // SAFETY: the slot's page, claimed: no other writer has it, and what a reader copies of it meanwhile it
        // throws away.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data(slot), bytes.len()) };
        self.word(said).store(counter + 2, Ordering::Release);
    }

    /// Hold nothing, and no slot claimed: for whoever knows that no other process has it - one that died writing
    /// may have left a slot claimed for good.
    pub fn clear(&self) {
        for slot in 0..self.slots {
            self.word(Ring::said(slot)).store(0, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A page's bytes, as its key alone says them: what is read back is held to it.
    fn page_of(key: &Key, len: usize) -> Vec<u8> {
        let seed = key.hash();
        (0..len as u64).map(|i| u8::try_from((seed.wrapping_add(i.wrapping_mul(31))) % 251).unwrap()).collect()
    }

    fn file(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("pe2-sfs-ring-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_page_put_is_read_back_and_another_version_is_not() {
        let ring = Ring::private(1 << 20).unwrap();
        let key = Key { number: 7, version: 3, page: 2 };
        let mut out = [0u8; PAGE];
        assert_eq!(ring.get(&key, &mut out), None);
        ring.put(&key, &page_of(&key, PAGE));
        assert_eq!(ring.get(&key, &mut out), Some(PAGE));
        assert_eq!(out[..], page_of(&key, PAGE)[..]);
        // A file's last page is shorter; put again, a page is as it was last put.
        ring.put(&key, &page_of(&key, 100));
        assert_eq!(ring.get(&key, &mut out), Some(100));
        assert_eq!(out[..100], page_of(&key, 100)[..]);
        // Another version of the file, another page, another file: not this one.
        for other in [Key { version: 4, ..key }, Key { page: 3, ..key }, Key { number: 8, ..key }] {
            assert_eq!(ring.get(&other, &mut out), None);
        }
    }

    #[test]
    fn what_is_put_past_what_it_holds_takes_the_place_of_what_was_used_longest_ago() {
        let ring = Ring::private(64 * (PAGE + SAID) as u64).unwrap();
        assert_eq!(ring.pages(), 64);
        let mut out = [0u8; PAGE];
        let key = |n: u64| Key { number: n, version: 1, page: 0 };
        for n in 0..10_000 {
            ring.put(&key(n), &page_of(&key(n), PAGE));
            // One read again and again stays, whatever is put meanwhile.
            if n >= 100 {
                assert!(ring.get(&key(50), &mut out).is_some(), "what is read often went after {n} pages put");
            } else if n == 99 {
                ring.put(&key(50), &page_of(&key(50), PAGE));
            }
        }
        // It holds as many as it has slots for at most, each as it was put.
        let held: Vec<u64> = (0..10_000).filter(|n| ring.get(&key(*n), &mut out).is_some()).collect();
        assert!(held.len() <= 64 && held.len() >= 32, "{} held", held.len());
        for n in held {
            assert_eq!(ring.get(&key(n), &mut out), Some(PAGE));
            assert_eq!(out[..], page_of(&key(n), PAGE)[..]);
        }
    }

    #[test]
    fn threads_that_put_and_read_at_once_read_whole_pages_or_none() {
        // Few slots, many keys: every slot is taken again and again under the readers.
        let ring = Ring::private(32 * (PAGE + SAID) as u64).unwrap();
        let stop = AtomicBool::new(false);
        let (hits, misses) = (AtomicU64::new(0), AtomicU64::new(0));
        std::thread::scope(|scope| {
            for thread in 0..4u64 {
                let (ring, stop, hits, misses) = (&ring, &stop, &hits, &misses);
                scope.spawn(move || {
                    let mut out = [0u8; PAGE];
                    let mut n = thread;
                    while !stop.load(Ordering::Relaxed) {
                        n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                        let key = Key { number: (n >> 33) % 200, version: 1, page: (n >> 20) % 3 };
                        let len = 1 + usize::try_from(key.hash() % PAGE as u64).unwrap();
                        if let Some(read) = ring.get(&key, &mut out) {
                            assert_eq!(read, len);
                            assert!(out[..read] == page_of(&key, len)[..], "a page read that was never put");
                            hits.fetch_add(1, Ordering::Relaxed);
                        } else {
                            ring.put(&key, &page_of(&key, len));
                            misses.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
            stop.store(true, Ordering::Relaxed);
        });
        assert!(hits.load(Ordering::Relaxed) > 0 && misses.load(Ordering::Relaxed) > 0);
    }

    #[test]
    #[ignore = "run by two_processes_share_one_ring, as its second process"]
    fn the_second_process() {
        let Ok(path) = std::env::var("PE2_SFS_RING_SECOND") else { return };
        let ring = Ring::open(Path::new(&path), 16 << 20).unwrap();
        let mut out = [0u8; PAGE];
        // What the first put, read here; and pages of its own, for the first to read.
        for n in 0..50 {
            let key = Key { number: n, version: 1, page: 0 };
            assert_eq!(ring.get(&key, &mut out), Some(PAGE));
            assert_eq!(out[..], page_of(&key, PAGE)[..]);
            let key = Key { number: n, version: 2, page: 0 };
            ring.put(&key, &page_of(&key, PAGE));
        }
    }

    #[test]
    fn two_processes_share_one_ring() {
        let path = file("processes");
        // Far more slots than pages: none of these takes another's.
        let ring = Ring::open(&path, 16 << 20).unwrap();
        for n in 0..50 {
            let key = Key { number: n, version: 1, page: 0 };
            ring.put(&key, &page_of(&key, PAGE));
        }
        let second = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "ring::tests::the_second_process"])
            .env("PE2_SFS_RING_SECOND", &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(second.success());
        let mut out = [0u8; PAGE];
        for n in 0..50 {
            let key = Key { number: n, version: 2, page: 0 };
            assert_eq!(ring.get(&key, &mut out), Some(PAGE));
            assert_eq!(out[..], page_of(&key, PAGE)[..]);
        }
        // Opened at another length, it is another ring: nothing of this one is read as its.
        drop(ring);
        let other = Ring::open(&path, 2 << 20).unwrap();
        assert!(other.pages() < 1000);
        assert_eq!(other.get(&Key { number: 1, version: 2, page: 0 }, &mut out), None);
        drop(other);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_slot_left_claimed_is_a_miss_until_the_ring_is_cleared() {
        let ring = Ring::private(4 * (PAGE + SAID) as u64).unwrap();
        assert_eq!(ring.pages(), 4);
        let key = |n: u64| Key { number: n, version: 1, page: 0 };
        let mut out = [0u8; PAGE];
        for n in 0..4 {
            ring.put(&key(n), &page_of(&key(n), PAGE));
        }
        // As a process that died writing leaves one: its counter odd for good.
        let slot = ring.ways(&key(0)).find(|slot| ring.is(*slot, &key(0))).unwrap();
        ring.word(Ring::said(slot)).fetch_add(1, Ordering::AcqRel);
        assert_eq!(ring.get(&key(0), &mut out), None);
        // The others go on: read, and taken for what is put next.
        assert!(ring.get(&key(1), &mut out).is_some());
        for n in 10..40 {
            ring.put(&key(n), &page_of(&key(n), PAGE));
            assert_eq!(ring.get(&key(n), &mut out), Some(PAGE));
        }
        assert_eq!(ring.word(Ring::said(slot)).load(Ordering::Acquire) % 2, 1, "the claimed slot was taken");
        ring.clear();
        assert_eq!(ring.get(&key(39), &mut out), None);
        for n in 0..4 {
            ring.put(&key(n), &page_of(&key(n), PAGE));
        }
        assert!((0..4).all(|n| ring.get(&key(n), &mut out).is_some()), "every slot is one again");
    }

    #[test]
    #[ignore = "a measurement"]
    fn bench_ring() {
        let path = file("bench");
        for (name, ring) in [("memory", Ring::private(256 << 20).unwrap()), ("a file", Ring::open(&path, 256 << 20).unwrap())] {
            let key = |n: u64| Key { number: n % 5000, version: 1, page: n / 5000 };
            let page = page_of(&key(0), PAGE);
            let mut out = [0u8; PAGE];
            let first = std::time::Instant::now();
            for n in 0..50_000 {
                ring.put(&key(n), &page);
            }
            let first = first.elapsed();
            // Again, into memory the system has given by now.
            let put = std::time::Instant::now();
            for n in 0..50_000 {
                ring.put(&key(n), &page);
            }
            let put = put.elapsed();
            let hit = std::time::Instant::now();
            let mut hits = 0u64;
            for round in 0..20u64 {
                for n in 0..50_000 {
                    hits += u64::from(ring.get(&key((n * 7919 + round) % 50_000), &mut out).is_some());
                    std::hint::black_box(&out);
                }
            }
            let hit = hit.elapsed();
            let miss = std::time::Instant::now();
            for n in 0..1_000_000u64 {
                assert!(ring.get(&Key { number: n, version: 9, page: 0 }, &mut out).is_none());
            }
            let miss = miss.elapsed();
            // The same few pages again and again: what a program reading one file does.
            let hot = std::time::Instant::now();
            for n in 0..1_000_000u64 {
                std::hint::black_box(ring.get(&key(n % 16), &mut out));
                std::hint::black_box(&out);
            }
            let hot = hot.elapsed();
            eprintln!("{name}: the same sixteen pages read again and again {:.0} ns each", hot.as_secs_f64() * 1e9 / 1_000_000.0);
            eprintln!(
                "{name}: {} pages; a page put {:.0} ns the first time, {:.0} ns after; a page read {:.0} ns ({} of 1,000,000 found); a miss {:.0} ns",
                ring.pages(),
                first.as_secs_f64() * 1e9 / 50_000.0,
                put.as_secs_f64() * 1e9 / 50_000.0,
                hit.as_secs_f64() * 1e9 / 1_000_000.0,
                hits,
                miss.as_secs_f64() * 1e9 / 1_000_000.0
            );
        }
        std::fs::remove_file(&path).unwrap();
        // Beside it: the same reads asked of the system, of a file it has in memory.
        let page = [7u8; PAGE];
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        for _ in 0..50_000 {
            std::io::Write::write_all(&mut &file, &page).unwrap();
        }
        let read_at = |at: u64, out: &mut [u8; PAGE]| {
            #[cfg(unix)]
            std::os::unix::fs::FileExt::read_exact_at(&file, out, at * PAGE as u64).unwrap();
            #[cfg(windows)]
            assert_eq!(std::os::windows::fs::FileExt::seek_read(&file, out, at * PAGE as u64).unwrap(), PAGE);
        };
        let mut out = [0u8; PAGE];
        for n in 0..50_000 {
            read_at(n, &mut out);
        }
        let asked = std::time::Instant::now();
        for n in 0..1_000_000u64 {
            read_at((n * 7919) % 50_000, &mut out);
            std::hint::black_box(&out);
        }
        let asked = asked.elapsed();
        let hot = std::time::Instant::now();
        for n in 0..1_000_000u64 {
            read_at(n % 16, &mut out);
            std::hint::black_box(&out);
        }
        let hot = hot.elapsed();
        eprintln!(
            "asked of the system, a file it has in memory: a page read {:.0} ns; the same sixteen again and again {:.0} ns each",
            asked.as_secs_f64() * 1e9 / 1_000_000.0,
            hot.as_secs_f64() * 1e9 / 1_000_000.0
        );
        drop(file);
        std::fs::remove_file(&path).unwrap();
    }
}
