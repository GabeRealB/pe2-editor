//! Resource bundles, the payload of an opcode-4 chunk (`doc/BUNDLE_FORMAT.md`): fifty 16-byte directory entries - kind
//! (0 empty, 2 image, 3 data), sectors, length and RAM destination - then the resources, laid out as they are in RAM,
//! the first at 0x7F0. An image resource is byte for byte a palette chunk's payload.

use std::collections::BTreeMap;

const SLOTS: usize = 50;
const ENTRY: usize = 16;
const FIRST: usize = 0x7F0;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A bundle's used directory slots, each with its kind and bytes (cut short where the payload ends).
#[must_use]
pub fn resources(data: &[u8]) -> BTreeMap<usize, (u8, &[u8])> {
    let mut used = Vec::new();
    for n in 0..SLOTS {
        let at = n * ENTRY;
        let (Some(&kind), Some(length), Some(dest)) = (data.get(at), u32_at(data, at + 4), u32_at(data, at + 8)) else {
            break;
        };
        if kind != 0 {
            used.push((n, kind, length as usize, dest as usize));
        }
    }
    let base = used.iter().map(|&(_, _, _, dest)| dest).min().unwrap_or(0);
    used.into_iter()
        .map(|(n, kind, length, dest)| {
            let start = (FIRST + dest - base).min(data.len());
            (n, (kind, &data[start..(start + length).min(data.len())]))
        })
        .collect()
}
