//! Whether `lzss::encode` makes the discs' LZSS streams byte for byte.
//!
//! `cargo run --release -p formats --example lzss_identity -- <rom/USA>`
//!
//! Every stream of both discs' stage containers - a package chunk's payload, each strip of an image chunk (after its
//! work table, zero bytes between strips), a palette chunk's payload after its 16-byte header - is taken as stored
//! through its end token, decoded, encoded again and compared, on every core. It reports the streams of each kind
//! and how many came back the same, and names the first few that did not.

use formats::{cdf, lzss};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Package,
    Strip,
    Palette,
}

struct Stream {
    kind: Kind,
    origin: String,
    stored: Vec<u8>,
    data: Vec<u8>,
}

/// One stream from `start`, as stored through its end token, and what it decodes to.
fn stored_at(payload: &[u8], start: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let (data, end) = lzss::decode_stream(payload, start);
    (end > start).then(|| (payload[start..end].to_vec(), data))
}

/// Every stream of the discs under `roms`, each once.
fn streams(roms: &Path) -> Vec<Stream> {
    let (mut out, mut seen) = (Vec::new(), HashSet::new());
    for disc in ["disk1", "disk2"] {
        let dir = roms.join(disc);
        let mut containers: Vec<(String, Vec<u8>, Vec<cdf::FileEntry>)> = Vec::new();
        if let (Ok(hed), Ok(bytes)) = (std::fs::read(dir.join("STAGE0.HED")), std::fs::read(dir.join("STAGE0.CDF"))) {
            containers.push(("stage0".into(), bytes, cdf::stage0_files(&hed).expect("STAGE0.HED")));
        }
        for n in 1..=5 {
            if let Ok(bytes) = std::fs::read(dir.join(format!("STAGE{n}.CDF"))) {
                let files = cdf::stage_files(&mut bytes.as_slice()).expect("its folders");
                containers.push((format!("stage{n}"), bytes, files));
            }
        }
        for (stage, bytes, files) in &containers {
            for file in files {
                // A whole-file sequence has no chunks.
                let Ok(chunks) = cdf::chunks(&mut bytes.as_slice(), file.offset) else { continue };
                for (k, chunk) in chunks.iter().enumerate() {
                    let payload = chunk.payload(&mut bytes.as_slice()).expect("its payload");
                    let origin = format!("{disc}/{stage}/{}:{}#{k}", file.folder.unwrap_or(0), file.id);
                    let mut take = |kind, origin: String, (stored, data): (Vec<u8>, Vec<u8>)| {
                        if seen.insert(stored.clone()) {
                            out.push(Stream { kind, origin, stored, data });
                        }
                    };
                    match chunk.kind {
                        0 => {
                            if let Some(found) = stored_at(&payload, 0) {
                                take(Kind::Package, origin, found);
                            }
                        }
                        1 => {
                            let mut at = 0;
                            while at + 8 <= payload.len() {
                                let x = u16::from_le_bytes([payload[at], payload[at + 1]]);
                                at += 8;
                                if x == 0xFFFF {
                                    break;
                                }
                            }
                            for strip in 0.. {
                                while payload.get(at) == Some(&0) {
                                    at += 1;
                                }
                                let Some((stored, data)) = stored_at(&payload, at) else { break };
                                if data.is_empty() {
                                    break;
                                }
                                at += stored.len();
                                take(Kind::Strip, format!("{origin}/{strip}"), (stored, data));
                            }
                        }
                        2 if payload.len() > 16 => {
                            if let Some(found) = stored_at(&payload, 16) {
                                take(Kind::Palette, origin, found);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

fn main() {
    let roms = std::env::args().nth(1).unwrap_or_else(|| "../rom/USA".into());
    let streams = streams(Path::new(&roms));
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let per = streams.len().div_ceil(threads).max(1);
    let results: Vec<(Kind, bool, &str)> = std::thread::scope(|scope| {
        let handles: Vec<_> = streams
            .chunks(per)
            .map(|part| {
                scope.spawn(move || part.iter().map(|s| (s.kind, lzss::encode(&s.data) == s.stored, s.origin.as_str())).collect::<Vec<_>>())
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a worker")).collect()
    });
    let mut by: BTreeMap<Kind, (usize, usize)> = BTreeMap::new();
    for (kind, same, _) in &results {
        let e = by.entry(*kind).or_default();
        (e.0, e.1) = (e.0 + usize::from(*same), e.1 + 1);
    }
    for (kind, (same, total)) in &by {
        println!("{kind:?}: {same}/{total} the same");
    }
    let differing: Vec<&str> = results.iter().filter(|r| !r.1).map(|r| r.2).collect();
    for origin in differing.iter().take(10) {
        println!("  differs: {origin}");
    }
    if !differing.is_empty() {
        std::process::exit(1);
    }
}
