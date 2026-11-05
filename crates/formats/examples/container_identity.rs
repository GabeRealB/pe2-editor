//! Whether `cdf::pack_stage0`, `pack_folder` and `pack_stage` make the discs' stage containers byte for byte.
//!
//! `cargo run --release -p formats --example container_identity -- <rom/USA>`
//!
//! Each container is made again from what it holds as stored - each file's bytes, its stream records, a folder's
//! stored streams after its files - and compared with the disc's: STAGE0.HED and STAGE0.CDF, and every STAGE<N>.CDF.

use formats::cdf;
use std::path::Path;

const SECTOR: u64 = 0x800;

/// A file's bytes from its offset: its chunk chain, or (a whole-file sequence) up to `until`.
fn file_at(bytes: &[u8], offset: u64, until: u64) -> Vec<u8> {
    let end = cdf::chunks(&mut &bytes[..], offset)
        .ok()
        .and_then(|c| c.last().map(|l| l.offset + u64::from(l.sectors) * SECTOR))
        .unwrap_or(until);
    bytes[usize::try_from(offset).unwrap_or(0)..usize::try_from(end).unwrap_or(0)].to_vec()
}

/// Each file's place as the disc has it: its own bytes, or those of the earlier file at the same offset.
fn places<'a>(files: &[cdf::FileEntry], owned: &'a [(u32, Vec<u8>)]) -> Vec<(u32, cdf::Place<'a>)> {
    files
        .iter()
        .zip(owned)
        .enumerate()
        .map(|(k, (f, (id, bytes)))| match files[..k].iter().find(|e| e.offset == f.offset) {
            Some(first) => (*id, cdf::Place::Shares(first.id)),
            None => (*id, cdf::Place::Own(bytes)),
        })
        .collect()
}

fn main() {
    let roms = std::env::args().nth(1).unwrap_or_else(|| "../rom/USA".into());
    let mut failed = false;
    for disc in ["disk1", "disk2"] {
        let dir = Path::new(&roms).join(disc);
        // Stage 0: each file up to the next distinct place, its trailing data (the ending's stream) with it.
        let hed = std::fs::read(dir.join("STAGE0.HED")).expect("STAGE0.HED");
        let cdf0 = std::fs::read(dir.join("STAGE0.CDF")).expect("STAGE0.CDF");
        let files = cdf::stage0_files(&hed).expect("its files");
        let mut starts: Vec<u64> = files.iter().map(|f| f.offset).collect();
        starts.sort_unstable();
        starts.dedup();
        let owned: Vec<(u32, Vec<u8>)> = files
            .iter()
            .map(|f| {
                let next = starts.iter().copied().find(|&s| s > f.offset).unwrap_or(cdf0.len() as u64);
                (f.id, cdf0[usize::try_from(f.offset).unwrap_or(0)..usize::try_from(next).unwrap_or(0)].to_vec())
            })
            .collect();
        let borrowed = places(&files, &owned);
        let (h, c) = cdf::pack_stage0(&borrowed, &cdf::stage0_streams(&hed).expect("its streams"));
        let same = h == hed && c == cdf0;
        failed |= !same;
        println!("{disc}/STAGE0: {}", if same { "the same" } else { "differs" });
        for n in 1..=5 {
            let Ok(bytes) = std::fs::read(dir.join(format!("STAGE{n}.CDF"))) else { continue };
            let mut src: &[u8] = &bytes;
            let folders = cdf::stage_folders(&mut src).expect("its folders");
            let files = cdf::stage_files(&mut src).expect("its files");
            let mut made: Vec<(u32, Vec<u8>)> = Vec::new();
            for folder in &folders {
                let own: Vec<&cdf::FileEntry> = files.iter().filter(|f| f.folder == Some(folder.id)).collect();
                let end = folder.offset + folder.size;
                let owned: Vec<(u32, Vec<u8>)> = own.iter().map(|f| (f.id, file_at(&bytes, f.offset, end))).collect();
                let files_end = own
                    .iter()
                    .zip(&owned)
                    .map(|(f, (_, b))| f.offset + b.len() as u64)
                    .max()
                    .unwrap_or(folder.offset + SECTOR);
                let tail = &bytes[usize::try_from(files_end).unwrap_or(0)..usize::try_from(end).unwrap_or(0)];
                let entries: Vec<cdf::FileEntry> = own.iter().map(|f| **f).collect();
                let borrowed = places(&entries, &owned);
                let streams = cdf::folder_streams(&mut src, folder).expect("its streams");
                made.push((folder.id, cdf::pack_folder(&borrowed, &streams, tail)));
            }
            let borrowed: Vec<(u32, &[u8])> = made.iter().map(|(id, b)| (*id, b.as_slice())).collect();
            let same = cdf::pack_stage(&borrowed) == bytes;
            failed |= !same;
            println!("{disc}/STAGE{n}: {} ({} folders)", if same { "the same" } else { "differs" }, folders.len());
        }
    }
    if failed {
        std::process::exit(1);
    }
}
