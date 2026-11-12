//! The space and its trees, held to a map kept beside them - by one process, by threads, by two processes, and
//! across everything a process can stop at: a writer at each step of a transaction (`Space::fault`), a reader
//! before and after its pin, a reader killed while pinned, a page that is not the one pointed to, and a disk that
//! kept any part of what was last written.

use super::*;
use crate::tree;
use std::sync::atomic::AtomicUsize;

type Model = BTreeMap<Vec<u8>, Vec<u8>>;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pe2-sfs-space-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Rng(u64);

impl Rng {
    fn of(prefix: &str, n: u64) -> Rng {
        let seed = prefix.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |seed, byte| (seed ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3));
        Rng((seed ^ n.wrapping_mul(0xD6E8_FEB8_6659_FD93)) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

enum Change {
    Put(Vec<u8>, Vec<u8>),
    Remove(Vec<u8>),
}

/// One of a prefix's keys: some as long as a key may nearly be.
fn key(prefix: &str, n: u64) -> Vec<u8> {
    let mut key = format!("{prefix}/{n:05}").into_bytes();
    if n % 7 == 0 {
        key.extend(std::iter::repeat_n(b'k', 100 + usize::try_from(n * 37 % 600).unwrap()));
    }
    key
}

/// A value: mostly short, some longer than a leaf holds, a few of many pages, some of nothing.
fn value(rng: &mut Rng) -> Vec<u8> {
    let len = match rng.below(10) {
        0 => 0,
        1..=6 => rng.below(200),
        7 | 8 => 400 + rng.below(2000),
        _ => rng.below(20_000),
    };
    let seed = rng.next();
    (0..len).map(|i| u8::try_from(seed.wrapping_add(i * 31) % 251).unwrap()).collect()
}

/// The changes of a prefix's `n`th transaction, the same whoever makes them: some of its keys given values, some
/// removed, and the number of transactions made said last.
fn changes(prefix: &str, n: u64, keys: u64) -> Vec<Change> {
    let mut rng = Rng::of(prefix, n);
    let mut changes: Vec<Change> = (0..=rng.below(8))
        .map(|_| {
            let key = key(prefix, rng.below(keys));
            if rng.below(3) == 0 { Change::Remove(key) } else { Change::Put(key, value(&mut rng)) }
        })
        .collect();
    changes.push(Change::Put(format!("{prefix}.made").into_bytes(), (n + 1).to_le_bytes().to_vec()));
    changes
}

fn make(txn: &mut Txn<'_>, changes: &[Change]) -> Result<()> {
    for change in changes {
        match change {
            Change::Put(key, value) => tree::put(txn, 0, key, value)?,
            Change::Remove(key) => drop(tree::remove(txn, 0, key)?),
        }
    }
    Ok(())
}

fn note(model: &mut Model, changes: Vec<Change>) {
    for change in changes {
        match change {
            Change::Put(key, value) => drop(model.insert(key, value)),
            Change::Remove(key) => drop(model.remove(&key)),
        }
    }
}

/// A prefix's transactions from one on, made and committed one by one, and noted.
fn run(space: &Space, prefix: &str, range: std::ops::Range<u64>, keys: u64, model: &mut Model) {
    for n in range {
        let changes = changes(prefix, n, keys);
        let mut txn = space.write().unwrap();
        make(&mut txn, &changes).unwrap();
        txn.commit().unwrap();
        note(model, changes);
    }
}

/// What a prefix's first transactions leave.
fn after(prefix: &str, made: u64, keys: u64, model: &mut Model) {
    for n in 0..made {
        note(model, changes(prefix, n, keys));
    }
}

/// All a tree holds.
fn listed(pages: &impl Pages) -> Result<Model> {
    let mut all = Model::new();
    tree::scan(pages, 0, b"", &mut |key, value| {
        all.insert(key.to_vec(), value.to_vec());
        true
    })?;
    Ok(all)
}

/// Hold a tree to a map: all it lists, and what it says of keys asked one by one - those it has, and one it has not.
fn same(pages: &impl Pages, model: &Model) {
    let listed = listed(pages).unwrap();
    if &listed != model {
        let differ = model.keys().chain(listed.keys()).find(|key| model.get(*key) != listed.get(*key)).unwrap();
        panic!(
            "the tree lists {} keys, the map beside it holds {}; at {:?} the tree has {:?} bytes, the map {:?}",
            listed.len(),
            model.len(),
            String::from_utf8_lossy(&differ[..differ.len().min(24)]),
            listed.get(differ).map(Vec::len),
            model.get(differ).map(Vec::len)
        );
    }
    for (key, value) in model.iter().step_by(7) {
        assert_eq!(tree::get(pages, 0, key).unwrap().as_ref(), Some(value));
    }
    assert_eq!(tree::get(pages, 0, b"no such key").unwrap(), None);
}

/// Many transactions - some given up - against a map kept beside, readers held across them reading what they
/// began with; then all of it removed, and every page free again.
fn churn(space: &Space) {
    let mut rng = Rng::of("churn", 1);
    let mut model = Model::new();
    let mut held: Vec<(Snapshot<'_>, Model)> = Vec::new();
    for round in 0..3000u64 {
        let changes = changes("r", rng.next(), 300);
        let mut txn = space.write().unwrap();
        make(&mut txn, &changes).unwrap();
        if rng.below(10) == 0 {
            // Given up: nothing of it is seen.
            drop(txn);
        } else {
            note(&mut model, changes);
            if round % 64 == 0 {
                same(&txn, &model);
            }
            txn.commit().unwrap();
        }
        if rng.below(4) == 0 && held.len() < 3 {
            held.push((space.read().unwrap(), model.clone()));
        }
        if rng.below(4) == 0 && !held.is_empty() {
            let (snapshot, was) = held.swap_remove(usize::try_from(rng.below(held.len() as u64)).unwrap());
            same(&snapshot, &was);
        }
        if round % 64 == 0 {
            same(&space.read().unwrap(), &model);
        }
    }
    for (snapshot, was) in held.drain(..) {
        same(&snapshot, &was);
    }
    same(&space.read().unwrap(), &model);
    let mut txn = space.write().unwrap();
    for key in model.keys() {
        assert!(tree::remove(&mut txn, 0, key).unwrap());
    }
    txn.commit().unwrap();
    let snapshot = space.read().unwrap();
    assert_eq!(snapshot.root(0), 0, "a tree with nothing in it has no root");
    same(&snapshot, &Model::new());
    drop(snapshot);
    let usage = space.usage().unwrap();
    assert_eq!(usage.free, usage.pages, "every page ever written is free again");
}

#[test]
fn a_tree_is_what_a_map_kept_beside_it_holds() {
    churn(&Space::private().unwrap());
    let dir = folder("churn");
    churn(&Space::open(&dir).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pages_freed_are_taken_again_and_the_space_stays_as_small() {
    let space = Space::private().unwrap();
    let mut model = Model::new();
    run(&space, "p", 0..500, 100, &mut model);
    let settled = space.usage().unwrap();
    run(&space, "p", 500..3000, 100, &mut model);
    let later = space.usage().unwrap();
    same(&space.read().unwrap(), &model);
    assert!(later.pages <= settled.pages * 2, "{settled:?} grew to {later:?} over the same keys");
}

#[test]
fn readers_on_other_threads_read_one_commit_whole() {
    let dir = folder("threads");
    for space in [Space::private().unwrap(), Space::open(&dir).unwrap()] {
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    let mut last = 0u64;
                    while !stop.load(Ordering::Acquire) {
                        let snapshot = space.read().unwrap();
                        let said: Vec<u64> = (0..4)
                            .map(|i| {
                                let value = tree::get(&snapshot, 0, format!("group/{i}").as_bytes()).unwrap();
                                value.map_or(0, |value| u64::from_le_bytes(value[..8].try_into().unwrap()))
                            })
                            .collect();
                        assert!(said.iter().all(|n| *n == said[0]), "one commit's keys read apart: {said:?}");
                        assert!(said[0] >= last, "a commit read after a later one");
                        last = said[0];
                    }
                });
            }
            let mut model = Model::new();
            for n in 1..=1500u64 {
                let mut txn = space.write().unwrap();
                // With it, what moves pages about: freed ones are taken again under the readers.
                let changes = changes("t", n, 200);
                make(&mut txn, &changes).unwrap();
                for i in 0..4 {
                    // Of a length that changes, long ones among them.
                    let mut value = n.to_le_bytes().to_vec();
                    value.resize(8 + usize::try_from(n * 131 % 3000).unwrap(), 7);
                    tree::put(&mut txn, 0, format!("group/{i}").as_bytes(), &value).unwrap();
                    model.insert(format!("group/{i}").into_bytes(), value);
                }
                txn.commit().unwrap();
                note(&mut model, changes);
            }
            stop.store(true, Ordering::Release);
            same(&space.read().unwrap(), &model);
        });
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// This test program again, to run one of its tests as another process.
fn process(test: &str, variable: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--ignored", "--exact", &format!("space::tests::{test}")]).env(variable, dir);
    command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command
}

#[test]
#[ignore = "run by two_processes_write_one_space, as its second process"]
fn the_second_writer() {
    let Ok(dir) = std::env::var("PE2_SFS_SPACE_SECOND") else { return };
    let space = Space::open(Path::new(&dir)).unwrap();
    run_beside(&space, "second", 400, 200);
}

/// A prefix's transactions, the tree held every few of them to what they left of that prefix's keys - whoever else
/// writes the tree meanwhile, and takes the pages this one freed.
fn run_beside(space: &Space, prefix: &str, made: u64, keys: u64) -> Model {
    let mut model = Model::new();
    for n in 0..made {
        run(space, prefix, n..n + 1, keys, &mut model);
        if n % 8 == 0 {
            let all = listed(&space.read().unwrap()).unwrap();
            let mine: Model = all.into_iter().filter(|(key, _)| key.starts_with(prefix.as_bytes())).collect();
            assert!(mine == model, "{prefix}: after {n} transactions its keys are not as it left them");
        }
    }
    model
}

#[test]
fn two_processes_write_one_space() {
    let dir = folder("processes");
    let space = Space::open(&dir).unwrap();
    let mut second = process("the_second_writer", "PE2_SFS_SPACE_SECOND", &dir).spawn().unwrap();
    let mut model = run_beside(&space, "first", 400, 200);
    assert!(second.wait().unwrap().success());
    after("second", 400, 200, &mut model);
    same(&space.read().unwrap(), &model);
    drop(space);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The steps a writer may stop at.
const STEPS: [&str; 7] = [
    "make: a page taken",
    "free: one fewer said",
    "free: an entry written",
    "commit: before its page",
    "commit: its page written",
    "publish: half said",
    "commit: published",
];

#[test]
#[ignore = "run by a_writer_that_stops_at_any_step_leaves_its_last_commit, as the writer"]
fn the_writer_that_stops() {
    let (Ok(dir), Ok(at)) = (std::env::var("PE2_SFS_SPACE_STOPS"), std::env::var("PE2_SFS_SPACE_AT")) else { return };
    let (step, nth) = at.split_once('#').unwrap();
    let (step, nth) = (step.to_owned(), nth.parse::<usize>().unwrap());
    let space = Space::open(Path::new(&dir)).unwrap();
    let hits = AtomicUsize::new(0);
    space.set_faults(Arc::new(move |point| {
        if point == step && hits.fetch_add(1, Ordering::AcqRel) + 1 == nth {
            // As a process that is killed: nothing of it runs on.
            std::process::exit(3);
        }
    }));
    run(&space, "stops", 0..300, 150, &mut Model::new());
}

#[test]
fn a_writer_that_stops_at_any_step_leaves_its_last_commit() {
    let mut stopped = 0;
    for (i, step) in STEPS.iter().enumerate() {
        for nth in [1usize, 4, 60, 700] {
            let dir = folder(&format!("stops-{i}-{nth}"));
            let space = Space::open(&dir).unwrap();
            let mut model = Model::new();
            run(&space, "stays", 0..40, 150, &mut model);
            let mut writer = process("the_writer_that_stops", "PE2_SFS_SPACE_STOPS", &dir);
            let status = writer.env("PE2_SFS_SPACE_AT", format!("{step}#{nth}")).status().unwrap();
            assert!(status.success() || status.code() == Some(3), "{step} #{nth}: {status}");
            stopped += usize::from(!status.success());
            // Beside a process that has it attached, it is as its last commit left it: all of every transaction
            // the writer says it made, and nothing of the one it stopped in.
            let snapshot = space.read().unwrap();
            let made = tree::get(&snapshot, 0, b"stops.made").unwrap().map_or(0, |made| u64::from_le_bytes(made.try_into().unwrap()));
            assert!(status.success() == (made == 300), "{step} #{nth}: made {made}, {status}");
            after("stops", made, 150, &mut model);
            same(&snapshot, &model);
            drop(snapshot);
            // And the next writer goes on from there.
            run(&space, "stays", 40..120, 150, &mut model);
            same(&space.read().unwrap(), &model);
            drop(space);
            // Alone, and never had on disk: nothing of it is trusted.
            let alone = Space::open(&dir).unwrap();
            assert!(alone.fresh());
            assert_eq!(alone.read().unwrap().root(0), 0);
            drop(alone);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
    assert!(stopped >= STEPS.len() * 2, "the writer stopped in {stopped} runs only");
}

#[test]
fn a_reader_stopped_about_its_pin_reads_what_is_there_when_it_goes_on() {
    for step in ["read: before the pin", "read: pinned"] {
        let space = Space::private().unwrap();
        let mut model = Model::new();
        run(&space, "p", 0..60, 100, &mut model);
        let (reached, is_reached) = std::sync::mpsc::channel::<()>();
        let (go_on, may_go_on) = std::sync::mpsc::channel::<()>();
        let (reached, may_go_on, once) = (Mutex::new(reached), Mutex::new(may_go_on), AtomicBool::new(false));
        space.set_faults(Arc::new(move |point| {
            if point == step && !once.swap(true, Ordering::AcqRel) {
                lock(&reached).send(()).unwrap();
                lock(&may_go_on).recv().unwrap();
            }
        }));
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let snapshot = space.read().unwrap();
                (snapshot.commit().generation, listed(&snapshot).unwrap())
            });
            is_reached.recv().unwrap();
            // While it is stopped, the pages of the commit it read are freed, and - where nothing is pinned yet -
            // taken again.
            let before = space.usage().unwrap();
            run(&space, "p", 60..260, 100, &mut model);
            let now = space.usage().unwrap();
            go_on.send(()).unwrap();
            let (generation, read) = reader.join().unwrap();
            assert_eq!(generation, now.generation, "{step}: it read a commit that was no longer the one");
            assert!(read == model, "{step}: what it read is not what the last commit holds");
            if step == "read: pinned" {
                assert!(now.pages > before.pages + 100, "{step}: its pin held no page back: {before:?}, {now:?}");
            }
        });
        // Its pin is gone with it: pages are taken again.
        let before = space.usage().unwrap();
        run(&space, "p", 260..460, 100, &mut model);
        assert_eq!(space.usage().unwrap().pages, before.pages);
    }
}

#[test]
#[ignore = "run by a_reader_killed_while_pinned_holds_no_page_back, as the reader"]
fn the_reader_that_is_killed() {
    let Ok(dir) = std::env::var("PE2_SFS_SPACE_PINS") else { return };
    let space = Space::open(Path::new(&dir)).unwrap();
    let _pinned = space.read().unwrap();
    std::fs::write(Path::new(&dir).join("pinned"), b"").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn a_reader_killed_while_pinned_holds_no_page_back() {
    let dir = folder("pinned");
    let space = Space::open(&dir).unwrap();
    let mut model = Model::new();
    run(&space, "p", 0..60, 100, &mut model);
    let mut reader = process("the_reader_that_is_killed", "PE2_SFS_SPACE_PINS", &dir).spawn().unwrap();
    let waited = Instant::now();
    while !dir.join("pinned").exists() {
        assert!(waited.elapsed() < Duration::from_secs(30), "the reader never pinned");
        std::thread::sleep(Duration::from_millis(5));
    }
    // While it lives, what is freed after its pin is not taken again.
    run(&space, "p", 60..160, 100, &mut model);
    let held = space.usage().unwrap();
    run(&space, "p", 160..260, 100, &mut model);
    let held_on = space.usage().unwrap();
    assert!(held_on.pages > held.pages + 50, "its pin held no page back: {held:?}, {held_on:?}");
    reader.kill().unwrap();
    reader.wait().unwrap();
    std::thread::sleep(PROBE * 2);
    run(&space, "p", 260..660, 100, &mut model);
    assert_eq!(space.usage().unwrap().pages, held_on.pages, "a dead reader's pin held pages back");
    same(&space.read().unwrap(), &model);
    drop(space);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_page_that_is_not_the_one_pointed_to_is_said_to_everyone_and_kept_aside() {
    let dir = folder("damaged");
    let space = Space::open(&dir).unwrap();
    let beside = Space::open(&dir).unwrap();
    let mut model = Model::new();
    run(&space, "p", 0..40, 100, &mut model);
    let root = space.read().unwrap().root(0);
    // The generation that wrote the root's page, changed: as of a page taken again while it was still pointed to.
    // SAFETY: a page of the space, written here on purpose.
    unsafe { *space.page_at(root & NUMBER, false).unwrap().add(8) ^= 0x55 };
    match tree::get(&space.read().unwrap(), 0, b"p.made") {
        Err(Error::Damaged(why)) => assert!(why.contains("leads to one that says"), "{why}"),
        other => panic!("read through a page that is not the one pointed to: {other:?}"),
    }
    // Every process is told, whatever it does next.
    assert!(matches!(beside.read(), Err(Error::Damaged(why)) if why.contains("leads to one that says")));
    assert!(matches!(beside.write(), Err(Error::Damaged(_))));
    assert!(matches!(space.write(), Err(Error::Damaged(_))));
    drop((space, beside));
    // The next to have it alone begins anew, the damaged one kept aside.
    let anew = Space::open(&dir).unwrap();
    assert!(anew.fresh());
    assert!(dir.join("damaged/head").is_file() && dir.join("damaged/00000000.chunk").is_file());
    let mut model = Model::new();
    run(&anew, "p", 0..10, 100, &mut model);
    same(&anew.read().unwrap(), &model);
    drop(anew);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cell_that_is_not_within_its_page_is_said() {
    let space = Space::private().unwrap();
    let mut model = Model::new();
    run(&space, "p", 0..3, 100, &mut model);
    let root = space.read().unwrap().root(0);
    // Where the root's first cell is said to be: past the page's end.
    // SAFETY: a page of the space, written here on purpose.
    unsafe { std::slice::from_raw_parts_mut(space.page_at(root & NUMBER, false).unwrap().add(PAGE_HEAD + 12), 2).fill(0xFF) };
    let read = listed(&space.read().unwrap());
    assert!(matches!(&read, Err(Error::Damaged(why)) if why.contains("holds what is not within it")), "{:?}", read.map(|all| all.len()));
    assert!(matches!(space.write(), Err(Error::Damaged(_))));
}

#[test]
fn attached_to_alone_it_is_trusted_only_as_it_was_had_on_disk() {
    let dir = folder("trusted");
    let mut model = Model::new();
    {
        let space = Space::open(&dir).unwrap();
        assert!(space.fresh());
        run(&space, "p", 0..60, 100, &mut model);
        assert!(space.sync().unwrap());
    }
    for _ in 0..2 {
        // Had on disk and not changed since: as it was, however often.
        let space = Space::open(&dir).unwrap();
        assert!(!space.fresh());
        same(&space.read().unwrap(), &model);
    }
    {
        let space = Space::open(&dir).unwrap();
        assert!(dir.join("clean").exists());
        run(&space, "p", 60..61, 100, &mut model);
        assert!(!dir.join("clean").exists(), "said clean after a change");
        // Another that attaches meanwhile has it as it is.
        let beside = Space::open(&dir).unwrap();
        assert!(!beside.fresh());
        same(&beside.read().unwrap(), &model);
    }
    // Changed since it was had on disk, and nobody has it: nothing of it is trusted.
    let space = Space::open(&dir).unwrap();
    assert!(space.fresh());
    same(&space.read().unwrap(), &Model::new());
    drop(space);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The files of a space's pages, read.
fn pages_of(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let files = std::fs::read_dir(dir).unwrap().flatten().filter_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        (of_pages(&name) && entry.file_type().ok()?.is_file()).then(|| (name, std::fs::read(entry.path()).unwrap()))
    });
    files.collect()
}

#[test]
fn whatever_part_of_its_last_writes_a_disk_kept_it_reads_right_or_says_so() {
    let dir = folder("disk");
    let space = Space::open(&dir).unwrap();
    let mut model = Model::new();
    run(&space, "p", 0..80, 100, &mut model);
    assert!(space.sync().unwrap());
    let before = pages_of(&dir);
    run(&space, "p", 80..300, 100, &mut Model::new());
    space.flush().unwrap();
    let later = pages_of(&dir);
    drop(space);
    let (mut anew, mut right, mut said) = (0, 0, 0);
    for seed in 0..48u64 {
        let mut rng = Rng::of("disk", seed);
        let mixed = folder(&format!("disk-{seed}"));
        std::fs::create_dir_all(&mixed).unwrap();
        for (name, now) in &later {
            // A disk that kept, of each file: all of what was written since it was had on disk, none of it, or
            // any pages of it.
            let Some(was) = before.get(name) else {
                std::fs::write(mixed.join(name), now).unwrap();
                continue;
            };
            let of_list = name == "head" || name == "free";
            let kept = match (seed % 4, of_list) {
                (0, _) | (1, true) => 0,
                (3, false) => 1,
                _ => 2,
            };
            let mut bytes = Vec::with_capacity(now.len());
            for (was, now) in was.chunks(PAGE).zip(now.chunks(PAGE)) {
                let later = kept == 1 || (kept == 2 && rng.below(2) == 0);
                bytes.extend_from_slice(if later { now } else { was });
            }
            std::fs::write(mixed.join(name), bytes).unwrap();
        }
        // And said clean all the same, as it was when that was true.
        std::fs::write(mixed.join("clean"), &before["clean"]).unwrap();
        let space = Space::open(&mixed).unwrap();
        if space.fresh() {
            anew += 1;
        } else {
            // Trusted: then what it reads is what it held when it was had on disk - or it says that a page is not
            // the one pointed to. Never anything else.
            let mut model = model.clone();
            let read = listed(&space.read().unwrap()).and_then(|all| {
                assert!(all == model, "seed {seed}: trusted, and read as something it never held");
                for n in 80..120 {
                    let changes = changes("p", n, 100);
                    let mut txn = space.write()?;
                    make(&mut txn, &changes)?;
                    txn.commit()?;
                    note(&mut model, changes);
                }
                let all = listed(&space.read()?)?;
                assert!(all == model, "seed {seed}: written on, and read as something else");
                Ok(())
            });
            match read {
                Ok(()) => right += 1,
                Err(Error::Damaged(_)) => said += 1,
                Err(Error::Io(e)) => panic!("seed {seed}: {e}"),
            }
        }
        assert!(seed % 4 != 0 || right > 0, "what was had on disk whole was not trusted");
        drop(space);
        std::fs::remove_dir_all(&mixed).unwrap();
    }
    assert!(anew > 0 && right > 0 && said > 0, "begun anew {anew}, read right {right}, said damaged {said}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "a measurement"]
fn bench_tree() {
    let dir = folder("bench");
    for (name, space) in [("memory", Space::private().unwrap()), ("files", Space::open(&dir).unwrap())] {
        // Keys as a folder's names are: its number, then a name.
        let name_of = |n: u64| {
            let mut key = (n / 40).to_be_bytes().to_vec();
            key.extend_from_slice(format!("file-{:06}.toml", n.wrapping_mul(0x9E37_79B9) % 1_000_000).as_bytes());
            key
        };
        let value = [7u8; 64];
        let filled = Instant::now();
        for batch in 0..1000u64 {
            let mut txn = space.write().unwrap();
            for n in batch * 100..batch * 100 + 100 {
                tree::put(&mut txn, 0, &name_of(n), &value).unwrap();
            }
            txn.commit().unwrap();
        }
        let filled = filled.elapsed();
        let mut rng = Rng::of("bench", 1);
        let single = Instant::now();
        for _ in 0..20_000 {
            let mut txn = space.write().unwrap();
            tree::put(&mut txn, 0, &name_of(rng.below(100_000)), &value).unwrap();
            txn.commit().unwrap();
        }
        let single = single.elapsed();
        let pinned = Instant::now();
        for _ in 0..1_000_000 {
            drop(space.read().unwrap());
        }
        let pinned = pinned.elapsed();
        let looked = Instant::now();
        for _ in 0..200_000 {
            let snapshot = space.read().unwrap();
            assert!(tree::get(&snapshot, 0, &name_of(rng.below(100_000))).unwrap().is_some());
        }
        let looked = looked.elapsed();
        let listing = Instant::now();
        let mut names = 0;
        for _ in 0..20_000 {
            let folder = rng.below(2500).to_be_bytes();
            let snapshot = space.read().unwrap();
            tree::scan(&snapshot, 0, &folder, &mut |key, _| {
                names += usize::from(key.starts_with(&folder));
                key.starts_with(&folder)
            })
            .unwrap();
        }
        let listing = listing.elapsed();
        let usage = space.usage().unwrap();
        eprintln!(
            "{name}: 100,000 names put a hundred a commit {:.2} us each; one put and its commit {:.2} us; a pin taken and let go {:.0} ns; \
             a name looked up {:.2} us; a folder of 40 listed {:.2} us ({} names); {} pages, {} free",
            filled.as_secs_f64() * 1e6 / 100_000.0,
            single.as_secs_f64() * 1e6 / 20_000.0,
            pinned.as_secs_f64() * 1e9 / 1_000_000.0,
            looked.as_secs_f64() * 1e6 / 200_000.0,
            listing.as_secs_f64() * 1e6 / 20_000.0,
            names / 20_000,
            usage.pages,
            usage.free
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What keeping a project's model in trees would cost, before any of it is built: a commit of as many puts as an
/// edit changes keys, over as many trees as the model is in; what a reader pinned for long costs the space; and
/// what filling the trees whole takes.
#[test]
#[ignore = "a measurement"]
#[expect(clippy::cast_precision_loss, clippy::too_many_lines, reason = "a measurement, said as it is taken")]
fn bench_model() {
    use std::fmt::Write as _;
    // Keys as a model's are: a kind, then a path; values as long as a node of the stack, or an asset.
    let key = |n: u64| format!("p/stages/{:03}/rooms/{:04}/props/item-{:06}", n % 17, n % 1009, n).into_bytes();
    let fill = |space: &Space, trees: usize, each: u64, value: &[u8]| {
        for tree in 0..trees {
            for batch in 0..each / 1000 {
                let mut txn = space.write().unwrap();
                for n in batch * 1000..batch * 1000 + 1000 {
                    tree::put(&mut txn, tree, &key(n), value).unwrap();
                }
                txn.commit().unwrap();
            }
        }
    };
    let median = |mut took: Vec<Duration>| {
        took.sort_unstable();
        (took[took.len() / 2].as_secs_f64() * 1e6, took[took.len() * 99 / 100].as_secs_f64() * 1e6)
    };
    let dir = folder("bench-model");
    for (name, space) in [("memory", Space::private().unwrap()), ("files", Space::open(&dir).unwrap())] {
        for trees in [4usize, 12] {
            // 40,000 keys a tree, of 120 bytes each: what the stack of a project of that many entries is.
            let filled = Instant::now();
            fill(&space, trees, 40_000, &[7u8; 120]);
            let filled = filled.elapsed();
            let usage = space.usage().unwrap();
            let mut said = format!(
                "{name}, {trees} trees of 40,000: filled {:.2} us a put, {} pages;",
                filled.as_secs_f64() * 1e6 / (trees as f64 * 40_000.0),
                usage.pages
            );
            let mut rng = Rng::of("model", trees as u64);
            for puts in [8usize, 15, 30] {
                let took: Vec<Duration> = (0..3000)
                    .map(|_| {
                        let started = Instant::now();
                        let mut txn = space.write().unwrap();
                        for put in 0..puts {
                            tree::put(&mut txn, put % trees, &key(rng.below(40_000)), &[9u8; 120]).unwrap();
                        }
                        txn.commit().unwrap();
                        started.elapsed()
                    })
                    .collect();
                let (middle, most) = median(took);
                let _ = write!(said, " a commit of {puts} puts {middle:.1} us (99th {most:.1});");
            }
            eprintln!("{said}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    // A reader pinned through many commits: what the space grows by while it is, and after it let go.
    for commits in [1_000u64, 10_000] {
        let space = Space::private().unwrap();
        fill(&space, 4, 40_000, &[7u8; 120]);
        let mut rng = Rng::of("pinned", commits);
        let mut edit = |space: &Space| {
            let mut txn = space.write().unwrap();
            for put in 0..8 {
                tree::put(&mut txn, put % 4, &key(rng.below(40_000)), &[9u8; 120]).unwrap();
            }
            txn.commit().unwrap();
        };
        let before = space.usage().unwrap();
        for _ in 0..commits {
            edit(&space);
        }
        let unpinned = space.usage().unwrap();
        let pin = space.read().unwrap();
        for _ in 0..commits {
            edit(&space);
        }
        let pinned = space.usage().unwrap();
        drop(pin);
        for _ in 0..commits {
            edit(&space);
        }
        let after = space.usage().unwrap();
        eprintln!(
            "{commits} commits of 8 puts: {} pages before; +{} with nothing pinned; +{} with a reader pinned throughout \
             ({:.1} a commit); +{} more once it let go ({} listed free)",
            before.pages,
            unpinned.pages - before.pages,
            pinned.pages - unpinned.pages,
            (pinned.pages - unpinned.pages) as f64 / commits as f64,
            after.pages - pinned.pages,
            after.free
        );
    }
    // A model filled whole, in the order of its keys: 170,000 of them, a thousand a commit.
    let space = Space::private().unwrap();
    let mut keys: Vec<Vec<u8>> = (0..170_000u64).map(key).collect();
    keys.sort();
    let filled = Instant::now();
    for batch in keys.chunks(1000) {
        let mut txn = space.write().unwrap();
        for key in batch {
            tree::put(&mut txn, 0, key, &[7u8; 120]).unwrap();
        }
        txn.commit().unwrap();
    }
    let usage = space.usage().unwrap();
    let bytes: usize = keys.iter().map(|key| key.len() + 120).sum();
    eprintln!(
        "170,000 keys put in their order: {:.0} ms, {} pages ({:.0}% of them what was put)",
        filled.elapsed().as_secs_f64() * 1e3,
        usage.pages,
        bytes as f64 * 100.0 / (usage.pages as f64 * 4096.0)
    );
}
