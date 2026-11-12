//! Whether `cdf::pack_chunk` makes the discs' file chunks byte for byte.
//!
//! `cargo run --release -p formats --example cdf_identity -- <the folder of a release's discs>`
//!
//! Every chunk of both discs' stage containers is made again - an LZSS chunk from what it decodes to (a package's
//! image, a palette's colors after its 16-byte header, an image's column table and strips), any other from its
//! payload as stored - with the stored header's sector length and load address, and compared with the disc's
//! sectors. It reports each type and names the first few chunks that differ, exiting non-zero if any does.

use formats::{cdf, lzss};
use std::collections::BTreeMap;
use std::path::Path;

const SECTOR: usize = 0x800;

/// A chunk as stored, where it is, and whether it is its file's last.
struct Stored {
    origin: String,
    chunk: cdf::Chunk,
    last: bool,
    sectors: Vec<u8>,
    payload: Vec<u8>,
}

fn stored(roms: &Path) -> Vec<Stored> {
    let mut out = Vec::new();
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
                let count = chunks.len();
                for (k, chunk) in chunks.into_iter().enumerate() {
                    let at = usize::try_from(chunk.offset).expect("an offset");
                    let sectors = bytes[at..at + chunk.sectors as usize * SECTOR].to_vec();
                    let payload = chunk.payload(&mut bytes.as_slice()).expect("its payload");
                    let origin = format!("{disc}/{stage}/{}:{}#{k}", file.folder.unwrap_or(0), file.id);
                    out.push(Stored { origin, chunk, last: k + 1 == count, sectors, payload });
                }
            }
        }
    }
    out
}

/// What a chunk is made from: an LZSS chunk's content decoded and encoded again, any other's payload as stored.
fn payload_of(s: &Stored) -> Vec<u8> {
    let p = &s.payload;
    match s.chunk.kind {
        0 => lzss::encode(&lzss::decode_stream(p, 0).0),
        2 => {
            let mut out = p[..16].to_vec();
            out.extend(lzss::encode(&lzss::decode_stream(p, 16).0));
            out
        }
        1 => {
            let first = u32::from_le_bytes([p[4], p[5], p[6], p[7]]) as usize;
            cdf::image_payload(&p[..first], &lzss::decode_streams(p, first), s.chunk.sector_len)
        }
        _ => p.clone(),
    }
}

fn main() {
    let roms = std::env::args().nth(1).expect("the folder of a release's discs");
    let all = stored(Path::new(&roms));
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let per = all.len().div_ceil(threads).max(1);
    let results: Vec<(u8, bool, &str)> = std::thread::scope(|scope| {
        let handles: Vec<_> = all
            .chunks(per)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|s| {
                            let c = &s.chunk;
                            let made = cdf::pack_chunk(c.kind, s.last, c.sector_len, c.load, &payload_of(s));
                            (c.kind, made == s.sectors, s.origin.as_str())
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("a worker")).collect()
    });
    let mut by: BTreeMap<u8, (usize, usize)> = BTreeMap::new();
    for (kind, same, _) in &results {
        let e = by.entry(*kind).or_default();
        (e.0, e.1) = (e.0 + usize::from(*same), e.1 + 1);
    }
    for (kind, (same, total)) in &by {
        println!("{}: {same}/{total} the same", cdf::slot_type(*kind).unwrap_or("?"));
    }
    let differing: Vec<&str> = results.iter().filter(|r| !r.1).map(|r| r.2).collect();
    for origin in differing.iter().take(10) {
        println!("  differs: {origin}");
    }
    if !differing.is_empty() {
        std::process::exit(1);
    }
}
