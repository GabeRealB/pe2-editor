//! Resource bundles, the payload of an opcode-4 chunk: fifty 16-byte directory entries - kind
//! (0 empty, 2 image, 3 data), sectors, length and RAM destination - then the resources, laid out as they are in RAM,
//! the first at 0x7F0. An image resource is byte for byte a palette chunk's payload.
//!
//! Laid out again (`write`), resources follow one another from the first's RAM destination in slot order, each on a
//! 16-byte boundary with the gap filled with 0xFF, and the bundle ends in zeros at a sector boundary less the chunk
//! header - in one retail bundle of two a sector more (`Bundle::extra_sectors`). That reproduces every retail bundle.

use std::collections::BTreeMap;

const SLOTS: usize = 50;
const ENTRY: usize = 16;
const FIRST: usize = 0x7F0;
const SECTOR: usize = 2048;
/// The chunk header the extractor strips from what a bundle's sectors hold.
const CHUNK_HEADER: usize = 16;
const ALIGN: usize = 16;

/// A bundle as what makes it again: its used slots' kinds and bytes, the first's RAM destination, and the zero sectors
/// past its usual end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bundle {
    pub resources: BTreeMap<usize, (u8, Vec<u8>)>,
    pub base: u32,
    pub extra_sectors: usize,
}

/// A bundle's length for the bytes it holds: whole sectors, less the chunk header.
fn size(end: usize) -> usize {
    (end + CHUNK_HEADER).div_ceil(SECTOR) * SECTOR - CHUNK_HEADER
}

/// Read a bundle as what makes it again.
///
/// # Errors
/// It is not laid out as `write` lays it out.
pub fn parse(data: &[u8]) -> Result<Bundle, String> {
    let resources: BTreeMap<usize, (u8, Vec<u8>)> =
        resources(data).into_iter().map(|(n, (kind, bytes))| (n, (kind, bytes.to_vec()))).collect();
    let base = (0..SLOTS).filter(|n| data.get(n * ENTRY).is_some_and(|k| *k != 0)).filter_map(|n| u32_at(data, n * ENTRY + 8)).min().unwrap_or(0);
    let usual = write(&Bundle { resources: resources.clone(), base, extra_sectors: 0 });
    let extra_sectors = data.len().saturating_sub(usual.len()) / SECTOR;
    let bundle = Bundle { resources, base, extra_sectors };
    if write(&bundle) != data {
        return Err("the bundle is not laid out as written".into());
    }
    Ok(bundle)
}

/// Lay a bundle out: its directory, then its resources from `base` in slot order, each on a 16-byte boundary.
#[must_use]
pub fn write(bundle: &Bundle) -> Vec<u8> {
    let mut out = vec![0u8; FIRST];
    let mut dest = bundle.base;
    for (&slot, (kind, payload)) in &bundle.resources {
        let at = slot * ENTRY;
        if at + ENTRY > FIRST {
            continue;
        }
        out[at] = *kind;
        out[at + 4..at + 8].copy_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&dest.to_le_bytes());
        let padded = payload.len().div_ceil(ALIGN) * ALIGN;
        out.extend(payload);
        out.resize(out.len() + padded - payload.len(), 0xFF);
        dest += u32::try_from(padded).unwrap_or(u32::MAX);
    }
    let end = size(out.len()) + bundle.extra_sectors * SECTOR;
    out.resize(end, 0);
    out
}

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

#[cfg(test)]
mod tests {
    use super::{Bundle, parse, write};

    #[test]
    fn a_bundle_reads_back_as_written() {
        let mut bundle = Bundle { base: 0x8019_0000, ..Bundle::default() };
        bundle.resources.insert(1, (2, vec![1, 2, 3]));
        bundle.resources.insert(16, (3, b"CAP2 and more".to_vec()));
        let bytes = write(&bundle);
        assert_eq!((bytes.len() + 16) % 2048, 0);
        assert_eq!(parse(&bytes), Ok(bundle.clone()));
        let longer = Bundle { extra_sectors: 1, ..bundle };
        assert_eq!(parse(&write(&longer)), Ok(longer));
    }
}
