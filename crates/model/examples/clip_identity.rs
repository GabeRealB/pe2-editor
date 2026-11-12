//! Every clip under a folder (`*.clip`: a clip's bytes, first pose bank through its `AnimationSet`, as an extraction
//! stores them) decoded, written as USD and read back, and encoded again, which has to give its bytes back. A clip's address is its lowest pointer:
//! its first bank, or its records where it has none.
//!
//! `cargo run --release -p pe2-model --example clip_identity -- <a folder of clips>`

use std::path::{Path, PathBuf};

use pe2_model::clip::{self, SET_SIZE};
use pe2_model::clip_usd;
use pe2_model::skeleton::Joint;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "clip") {
            out.push(path);
        }
    }
}

fn address(bytes: &[u8]) -> Option<u32> {
    let set = bytes.len().checked_sub(SET_SIZE)?;
    (0..10)
        .filter(|at| *at != 1)
        .map(|at| u32::from_le_bytes(bytes[set + at * 4..set + at * 4 + 4].try_into().unwrap_or_default()))
        .filter(|pointer| *pointer != 0)
        .min()
}

fn main() {
    let dir = std::env::args().nth(1).expect("a folder of clips");
    let mut all = Vec::new();
    files(Path::new(&dir), &mut all);
    all.sort();
    let (mut exact, mut failed) = (0, 0);
    for path in &all {
        let bytes = std::fs::read(path).unwrap_or_default();
        let outcome = address(&bytes).ok_or_else(|| "no pointer".to_string()).and_then(|at| {
            let decoded = clip::decode(&bytes, at).map_err(|e| format!("decode: {e:#}"))?;
            // Any skeleton of as many joints carries it: a chain, each joint under the one before.
            let joints: Vec<Joint> = (0..decoded.tracks.len())
                .map(|n| Joint {
                    name: format!("j{n}"),
                    parent: n.checked_sub(1),
                    translation: [0; 3],
                    rotation: [0.0; 3],
                })
                .collect();
            let text = clip_usd::write(&decoded, &joints).map_err(|e| format!("write: {e:#}"))?;
            let decoded = clip_usd::read(&text).map_err(|e| format!("read: {e:#}"))?;
            let again = clip::encode(&decoded, at).map_err(|e| format!("encode: {e:#}"))?;
            if again == bytes {
                return Ok(());
            }
            let first = again.iter().zip(&bytes).position(|(a, b)| a != b).unwrap_or(again.len().min(bytes.len()));
            Err(format!("differs at byte {first:#X} ({} against {} bytes)", again.len(), bytes.len()))
        });
        match outcome {
            Ok(()) => exact += 1,
            Err(why) => {
                failed += 1;
                if failed <= 20 {
                    println!("{}: {why}", path.display());
                }
            }
        }
    }
    println!("{exact} of {} clips exact, {failed} not", all.len());
    if failed > 0 {
        std::process::exit(1);
    }
}
