//! What serving costs, measured: run by hand (`cargo test --release -p vfs --test bench -- --ignored --nocapture`),
//! each printing `BENCH` lines to compare between backends and between changes.
//!
//! - `scope`: a folder of files held in memory - many small, a few large - served by the backend `PE2_TEST_SCOPES`
//!   names (else the best suited here), so that only serving is measured: listed, read a first time and again.
//! - `mounts`: several scopes made one after the other in this process, and how long each takes to answer.
//! - `tree`: a project's tree read directly, with nothing serving it (`PE2_BENCH_PROJECT`): what looking a path up
//!   and reading a file cost in this crate alone, to tell them from what a backend and the system add.
//! - `folder`: a project's folder as something serves it now (`PE2_BENCH_FOLDER`): listed, its small files and its
//!   largest read a first time and again, files written into it and removed.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use project::content::Given;

fn timed<T>(work: impl FnOnce() -> T) -> (Duration, T) {
    let started = Instant::now();
    let made = work();
    (started.elapsed(), made)
}

#[expect(clippy::cast_precision_loss, reason = "a rate, as it is said")]
fn rate(bytes: u64, time: Duration) -> f64 {
    bytes as f64 / f64::from(1u32 << 20) / time.as_secs_f64().max(1e-9)
}

/// Every file under a folder, with its length as its folder lists it.
fn listed(dir: &Path, found: &mut Vec<(PathBuf, u64)>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            listed(&entry.path(), found);
        } else {
            found.push((entry.path(), meta.len()));
        }
    }
}

/// Read files whole, a megabyte at a time: how many bytes they held.
fn read(paths: &[PathBuf]) -> u64 {
    let mut buffer = vec![0u8; 1 << 20];
    let mut bytes = 0;
    for path in paths {
        let mut file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        loop {
            let count = file.read(&mut buffer).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            if count == 0 {
                break;
            }
            bytes += count as u64;
        }
    }
    bytes
}

/// Few of the files read, once: what a scope's backend is chosen for.
fn once() -> project::content::Use {
    project::content::Use::once()
}

fn backend() -> String {
    std::env::var("PE2_TEST_SCOPES").unwrap_or_else(|_| "first".into())
}

/// Scopes served by the backend `PE2_TEST_SCOPES` names, else the first of this system's that serves at a folder
/// (named, so that what is small is served all the same, not laid out).
fn scopes() -> vfs::scoped::Scopes {
    vfs::scoped::Scopes::install(|| {
        let here = std::env::temp_dir();
        let first = || vfs::backend::all().iter().find(|b| b.can().folder && b.available(&here).is_ok());
        let named = std::env::var("PE2_TEST_SCOPES").ok().or_else(|| first().map(|b| b.name().to_string()));
        vfs::scoped::Machine { scopes: named, copies: None }
    })
}

#[test]
#[ignore = "a benchmark, run by hand"]
fn scope() {
    let root = std::env::temp_dir().join(format!("pe2-bench-scope-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let small: Arc<[u8]> = Arc::from(vec![7u8; 8 << 10]);
    let large: Arc<[u8]> = Arc::from(vec![9u8; 8 << 20]);
    let mut files = BTreeMap::new();
    for dir in 0..40 {
        for file in 0..50 {
            files.insert(format!("small/d{dir:02}/f{file:02}.h"), Given::Bytes(small.clone()));
        }
    }
    for file in 0..16 {
        files.insert(format!("large/f{file:02}.bin"), Given::Bytes(large.clone()));
    }
    // One folder of many headers, as an SDK's is: where a name looked for and not there costs the most.
    let tiny: Arc<[u8]> = Arc::from(vec![1u8; 64]);
    for file in 0..2000 {
        files.insert(format!("wide/h{file:04}.h"), Given::Bytes(tiny.clone()));
    }
    let serving = scopes();
    let (served, folder) =
        timed(|| project::content::serve_folder(Some(&root), &files, &once(), &root.join("fallback")).expect("served"));
    let list = |dir: &Path| {
        let mut found = Vec::new();
        listed(dir, &mut found);
        found
    };
    let (list_first, all) = timed(|| list(&folder.at));
    let (list_again, _) = timed(|| list(&folder.at));
    let paths = |under: &str| -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = all.iter().map(|(p, _)| p.clone()).filter(|p| p.starts_with(folder.at.join(under))).collect();
        paths.sort();
        paths
    };
    let (small_paths, large_paths) = (paths("small"), paths("large"));
    let (small_first, small_bytes) = timed(|| read(&small_paths));
    let (small_again, _) = timed(|| read(&small_paths));
    let (large_first, large_bytes) = timed(|| read(&large_paths));
    let (large_again, _) = timed(|| read(&large_paths));
    println!(
        "BENCH scope {}: serve {served:.0?} | list {} files first {list_first:.0?} again {list_again:.0?} | {} small \
         ({} MiB) first {small_first:.0?} again {small_again:.0?} | large {} MiB first {:.0} MiB/s again {:.0} MiB/s",
        backend(),
        all.len(),
        small_paths.len(),
        small_bytes >> 20,
        large_bytes >> 20,
        rate(large_bytes, large_first),
        rate(large_bytes, large_again),
    );
    // Asked whether it is there, in the folder of many: names that are, names that are not, names in another case.
    let wide = folder.at.join("wide");
    let asked = |names: &[String]| names.iter().filter(|name| std::fs::metadata(wide.join(name)).is_ok()).count();
    let named = |name: &dyn Fn(u32) -> String| (0..2000).map(name).collect::<Vec<String>>();
    let kinds = [
        ("there", named(&|n| format!("h{n:04}.h"))),
        ("missing", named(&|n| format!("x{n:04}.h"))),
        ("in another case", named(&|n| format!("H{n:04}.H"))),
    ];
    for (what, names) in &kinds {
        let (first, found) = timed(|| asked(names));
        let (again, _) = timed(|| asked(names));
        println!(
            "BENCH scope {}: asked for 2000 names {what} among 2000: {found} found, first {first:.0?} again {again:.0?} \
             ({:.0} us each)",
            backend(),
            again.as_secs_f64() * 1e6 / 2000.0
        );
    }
    let (let_go, ()) = timed(|| {
        drop(folder);
        drop(serving);
    });
    println!("BENCH scope {}: let go in {let_go:.0?}", backend());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "a benchmark, run by hand"]
fn mounts() {
    let base = std::env::temp_dir().join(format!("pe2-bench-mounts-{}", std::process::id()));
    let files = BTreeMap::from([("a.h".to_string(), Given::Bytes(Arc::from(&b"#define A 1"[..])))]);
    let serving = scopes();
    let mut kept = Vec::new();
    for nth in 0..4 {
        let root = base.join(format!("p{nth}"));
        std::fs::create_dir_all(&root).unwrap();
        let (served, folder) =
            timed(|| project::content::serve_folder(Some(&root), &files, &once(), &root.join("fallback")).expect("served"));
        let (answered, read) = timed(|| std::fs::read(folder.at.join("a.h")).map(|bytes| bytes.len()));
        println!("BENCH mounts {}: mount {nth} served in {served:.1?}, read {read:?} in {answered:.1?}", backend());
        kept.push(folder);
    }
    drop(kept);
    drop(serving);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[ignore = "a benchmark, run by hand"]
fn folder() {
    let Ok(folder) = std::env::var("PE2_BENCH_FOLDER") else {
        println!("BENCH folder: set PE2_BENCH_FOLDER to a project's served folder");
        return;
    };
    let (root, label) = (PathBuf::from(folder), std::env::var("PE2_BENCH_LABEL").unwrap_or_default());
    let list = || {
        let mut found = Vec::new();
        listed(&root, &mut found);
        found.retain(|(path, _)| !path.components().any(|step| step.as_os_str().to_string_lossy().starts_with("zz_")));
        found.sort();
        found
    };
    let (list_first, all) = timed(list);
    let (list_again, _) = timed(list);
    let total: u64 = all.iter().map(|(_, len)| len).sum();
    println!(
        "BENCH folder {label} list: {} files, {} MiB: first {list_first:.2?} again {list_again:.2?}",
        all.len(),
        total >> 20
    );
    let small: Vec<PathBuf> = all.iter().filter(|(_, len)| *len <= 64 << 10).take(3000).map(|(p, _)| p.clone()).collect();
    let (small_first, small_bytes) = timed(|| read(&small));
    let (small_again, _) = timed(|| read(&small));
    println!(
        "BENCH folder {label} small: {} files, {} KiB: first {small_first:.2?} again {small_again:.2?}",
        small.len(),
        small_bytes >> 10
    );
    if let Some((largest, len)) = all.iter().max_by_key(|(_, len)| *len).cloned() {
        let (first, _) = timed(|| read(std::slice::from_ref(&largest)));
        let (again, _) = timed(|| read(std::slice::from_ref(&largest)));
        println!(
            "BENCH folder {label} large: {} MiB: first {:.0} MiB/s again {:.0} MiB/s",
            len >> 20,
            rate(len, first),
            rate(len, again)
        );
    }
    // Asked whether it is there, as a compiler looking for a header along its paths does: names that are, names that
    // are not, and names written in another case (there on Windows, not elsewhere).
    let asked = |paths: &[PathBuf]| paths.iter().filter(|path| std::fs::metadata(path).is_ok()).count();
    let renamed = |change: &dyn Fn(&str) -> String| -> Vec<PathBuf> {
        let named = small.iter().filter_map(|path| Some(path.with_file_name(change(path.file_name()?.to_str()?))));
        named.collect()
    };
    let (missing, cased) = (renamed(&|name| format!("zz_no_{name}")), renamed(&|name| name.to_uppercase()));
    for (what, paths) in [("there", &small), ("missing", &missing), ("in another case", &cased)] {
        let (first, found) = timed(|| asked(paths));
        let (again, _) = timed(|| asked(paths));
        println!(
            "BENCH folder {label} asked for {} names {what}: {found} found, first {first:.2?} again {again:.2?} ({:.0} us each)",
            paths.len(),
            again.as_secs_f64() * 1e6 / f64::from(u32::try_from(paths.len().max(1)).unwrap_or(u32::MAX))
        );
    }
    if std::env::var_os("PE2_BENCH_WRITES").is_none() {
        return;
    }
    // Written, given time to land in the project, then removed: how long each takes the program, and the project.
    let dir = root.join("zz_bench");
    let payload = vec![5u8; 4 << 10];
    let (written, ()) = timed(|| {
        std::fs::create_dir_all(&dir).expect("folder made");
        for nth in 0..200 {
            std::fs::write(dir.join(format!("f{nth:03}.bin")), &payload).expect("written");
        }
    });
    let megabyte = vec![3u8; 1 << 20];
    let (big, ()) = timed(|| {
        use std::io::Write;
        let mut file = std::fs::File::create(dir.join("big.bin")).expect("made");
        for _ in 0..64 {
            file.write_all(&megabyte).expect("written");
        }
    });
    println!("BENCH folder {label} write: 200 files of 4 KiB in {written:.2?}, 64 MiB at {:.0} MiB/s", rate(64 << 20, big));
    std::thread::sleep(Duration::from_secs(4));
    let mut back = Vec::new();
    listed(&dir, &mut back);
    println!(
        "BENCH folder {label} written back: {} files, {} bytes (201 files, {} bytes written)",
        back.len(),
        back.iter().map(|(_, len)| len).sum::<u64>(),
        200 * (4 << 10) + (64u64 << 20)
    );
    let (removed, ()) = timed(|| {
        for (path, _) in &back {
            std::fs::remove_file(path).expect("removed");
        }
        std::fs::remove_dir(&dir).expect("folder removed");
    });
    println!("BENCH folder {label} remove: 201 files and the folder in {removed:.2?}");
}

#[test]
#[ignore = "a benchmark, run by hand"]
fn tree() {
    let Ok(root) = std::env::var("PE2_BENCH_PROJECT") else {
        println!("BENCH tree: set PE2_BENCH_PROJECT to a project's folder");
        return;
    };
    let project = project::Project::load(Path::new(&root)).expect("a project");
    let (indexed, index) = timed(|| project::index::Index::without_store(&project).expect("indexed"));
    let (made, served) = timed(|| vfs::Served::of(&index));
    let tree = served.tree();
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut folders = vec![String::new()];
    while let Some(folder) = folders.pop() {
        for (name, _, node) in tree.entries(&folder).unwrap_or_default() {
            let path = if folder.is_empty() { name } else { format!("{folder}/{name}") };
            match &node.what {
                vfs::What::Folder(_) => folders.push(path),
                vfs::What::File(_) => files.push((path, node.size)),
            }
        }
    }
    files.sort();
    let (looked, found) = timed(|| files.iter().filter(|(path, _)| tree.at(path).is_some()).count());
    let small: Vec<&(String, u64)> = files.iter().filter(|(_, size)| *size <= 64 << 10).take(3000).collect();
    let read_all = |paths: &[&(String, u64)]| {
        let mut bytes = 0u64;
        for (path, size) in paths {
            let (ino, _) = tree.at(path).expect("there");
            let mut at = 0;
            while at < *size {
                let read = tree.read(ino, at, 1 << 20).expect("read");
                if read.is_empty() {
                    break;
                }
                at += read.len() as u64;
            }
            bytes += at;
        }
        bytes
    };
    let (small_first, small_bytes) = timed(|| read_all(&small));
    let (small_again, _) = timed(|| read_all(&small));
    let largest: Vec<&(String, u64)> = files.iter().max_by_key(|(_, size)| *size).into_iter().collect();
    let (large_first, large_bytes) = timed(|| read_all(&largest));
    let (large_again, _) = timed(|| read_all(&largest));
    #[expect(clippy::cast_precision_loss, reason = "a mean, as it is said")]
    let each = |time: Duration, count: usize| time.as_secs_f64() * 1e6 / count.max(1) as f64;
    println!(
        "BENCH tree: indexed in {indexed:.2?}, tree of {} entries made in {made:.0?} | {found} lookups {:.2} us each | \
         {} small files ({} KiB) read first {:.0} us each, again {:.0} us each | large {} MiB first {:.0} MiB/s again \
         {:.0} MiB/s",
        tree.len(),
        each(looked, found),
        small.len(),
        small_bytes >> 10,
        each(small_first, small.len()),
        each(small_again, small.len()),
        large_bytes >> 20,
        rate(large_bytes, large_first),
        rate(large_bytes, large_again),
    );
}
