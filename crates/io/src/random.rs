//! Randomness, shaped like `wasi:random`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Where randomness comes from.
pub trait Random: Send + Sync {
    /// Bytes filled with randomness.
    fn fill(&self, bytes: &mut [u8]);

    /// A random number.
    fn u64(&self) -> u64 {
        let mut bytes = [0; 8];
        self.fill(&mut bytes);
        u64::from_le_bytes(bytes)
    }
}

/// The system's randomness.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRandom;

#[allow(clippy::disallowed_methods, reason = "the system's randomness: what every other reads through")]
impl Random for SystemRandom {
    fn fill(&self, bytes: &mut [u8]) {
        // The system with no randomness to give cannot be run on: nothing that needs it can go on.
        getrandom::fill(bytes).unwrap_or_else(|why| panic!("the system gives no randomness: {why}"));
    }
}

/// Randomness from a seed: the same seed, the same numbers, in the order they are asked for - what a test repeats.
/// (`SplitMix64`: each number a step of a counter, mixed.)
#[derive(Debug)]
pub struct Seeded(AtomicU64);

impl Seeded {
    #[must_use]
    pub fn new(seed: u64) -> Seeded {
        Seeded(AtomicU64::new(seed))
    }
}

impl Random for Seeded {
    fn fill(&self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let number = self.u64().to_le_bytes();
            chunk.copy_from_slice(&number[..chunk.len()]);
        }
    }

    fn u64(&self) -> u64 {
        let mut z = self.0.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed).wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's fixtures: the machine's time, places, files and environment, which nothing tested reads"
)]
mod tests {
    use super::{Random, Seeded};

    #[test]
    fn a_seed_gives_the_same_numbers_again() {
        let (a, b) = (Seeded::new(7), Seeded::new(7));
        let numbers: Vec<u64> = (0..4).map(|_| a.u64()).collect();
        assert_eq!(numbers, (0..4).map(|_| b.u64()).collect::<Vec<_>>());
        assert_ne!(numbers[0], numbers[1]);
        let mut bytes = [0; 11];
        Seeded::new(7).fill(&mut bytes);
        assert_eq!(bytes[..8], numbers[0].to_le_bytes(), "bytes as the numbers give them");
    }
}
