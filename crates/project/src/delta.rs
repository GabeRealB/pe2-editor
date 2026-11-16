//! What an override keeps of a file it changes: what it changes of the version it was made from (its base, named by
//! its SHA-1), never the whole of it - so that what a layer ships follows its edit, not the file. Three forms, the
//! first that gives the new file back exactly:
//!
//! - **Fields** of a TOML file: each field set or removed, by its path (`merge::fields::Step`), set in the base's
//!   document as a merge sets it - its layout and comments kept.
//! - **Lines** of a text: each run of lines replaced, by where it is in the base and how many it replaces, and the
//!   lines it becomes, as they are.
//! - **Bytes**: `pe2 delta 1`, runs copied from the base and bytes added, in order, kept as an object in the store.
//!
//! A delta is only ever applied to its base: whoever reads it has the base's bytes, by their SHA-1.

use std::fmt::Write as _;

use anyhow::{Context, Result, bail, ensure};

use crate::merge::fields::{self, Step};

/// A field a TOML file sets of its base: its path, and what it is (none: removed).
#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    pub at: Vec<Step>,
    pub to: Option<toml::Value>,
}

/// The fields `new` sets of `base`, where both are TOML tables and setting them in `base`'s document gives `new`
/// exactly; none otherwise (another layout, comments moved: what lines say).
#[must_use]
pub fn fields(base: &str, new: &str) -> Option<Vec<Set>> {
    let (from, to): (toml::Table, toml::Table) = (toml::from_str(base).ok()?, toml::from_str(new).ok()?);
    let sets: Vec<Set> =
        fields::diff(&from, &to).into_iter().map(|change| Set { at: change.path, to: change.value }).collect();
    (apply_fields(base, &sets).as_deref() == Some(new)).then_some(sets)
}

/// `base` with these fields set, as a merge sets them; none where it is no TOML document.
#[must_use]
pub fn apply_fields(base: &str, sets: &[Set]) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = base.parse().ok()?;
    for set in sets {
        fields::set_in_table(doc.as_table_mut(), &set.at, set.to.as_ref());
    }
    Some(doc.to_string())
}

/// Lines with their endings, so that joining them gives the text back.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The lines `new` changes of `base`: each run replaced, as `@@ <first line> <lines replaced> <lines it becomes>`, then
/// those lines as they are, endings and all - always `new` again, applied to `base`.
#[must_use]
pub fn lines_of(base: &str, new: &str) -> String {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (from, to) = (lines(base), lines(new));
    let mut out = String::new();
    for op in capture_diff_slices(Algorithm::Myers, &from, &to) {
        let (at, removed, added) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, .. } => (old_index, old_len, 0..0),
            DiffOp::Insert { old_index, new_index, new_len } => (old_index, 0, new_index..new_index + new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index, old_len, new_index..new_index + new_len)
            }
        };
        let _ = writeln!(out, "@@ {at} {removed} {}", added.len());
        for line in &to[added] {
            out.push_str(line);
        }
    }
    out
}

/// `base` with the lines `delta` changes changed (`lines_of`).
///
/// # Errors
/// The delta is none of `lines_of`, or names lines `base` has not.
pub fn apply_lines(base: &str, delta: &str) -> Result<String> {
    let from = lines(base);
    let mut said = lines(delta).into_iter();
    let (mut out, mut kept) = (String::new(), 0usize);
    while let Some(head) = said.next() {
        let numbers: Vec<usize> = head
            .strip_prefix("@@ ")
            .context("a run of lines that does not say where it is")?
            .trim_end()
            .split(' ')
            .map(str::parse)
            .collect::<Result<_, _>>()?;
        let [at, removed, added] = numbers[..] else { bail!("a run of lines said by {} numbers", numbers.len()) };
        ensure!(at >= kept && at + removed <= from.len(), "a run of lines at {at} of {}", from.len());
        out.extend(from[kept..at].iter().copied());
        for _ in 0..added {
            out.push_str(said.next().context("a run of lines that ends early")?);
        }
        kept = at + removed;
    }
    out.extend(from[kept..].iter().copied());
    Ok(out)
}

/// What a bytes delta's text begins with.
const MAGIC: &[u8] = b"pe2 delta 1\n";
/// The windows of the base runs are looked for by, and how far apart they start.
const WINDOW: usize = 32;
const STEP: usize = 16;
const COPY: u8 = 0;
const ADD: u8 = 1;

fn varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(u8::try_from(n & 0x7f).unwrap_or_default() | 0x80);
        n >>= 7;
    }
    out.push(u8::try_from(n).unwrap_or_default());
}

fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at).context("a delta that ends early")?;
        *at += 1;
        n |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(n);
        }
    }
    bail!("a number of a delta that never ends")
}

/// The hash of a window, kept as it slides a byte at a time.
fn window_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |h, &b| h.wrapping_mul(PRIME).wrapping_add(u64::from(b)))
}
const PRIME: u64 = 1_099_511_628_211;

/// The delta of `new` against `base` (`pe2 delta 1`): runs of `base` copied - each found by a window of it, and grown
/// both ways as far as the two agree - and the bytes between added. Linear in the two lengths.
#[must_use]
pub fn bytes_of(base: &[u8], new: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    varint(&mut out, new.len() as u64);
    // Where each window of the base starts, every STEP bytes, by its hash: the first of each.
    let mut starts: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    if base.len() >= WINDOW {
        for at in (0..=base.len() - WINDOW).step_by(STEP) {
            starts.entry(window_hash(&base[at..at + WINDOW])).or_insert(at);
        }
    }
    let high = (0..WINDOW - 1).fold(1u64, |p, _| p.wrapping_mul(PRIME));
    let (mut added_from, mut at) = (0usize, 0usize);
    let mut hash = (new.len() >= WINDOW).then(|| window_hash(&new[..WINDOW]));
    let add = |out: &mut Vec<u8>, run: &[u8]| {
        if !run.is_empty() {
            out.push(ADD);
            varint(out, run.len() as u64);
            out.extend_from_slice(run);
        }
    };
    while let Some(h) = hash.filter(|_| at + WINDOW <= new.len()) {
        let found = starts.get(&h).copied().filter(|&from| base[from..from + WINDOW] == new[at..at + WINDOW]);
        if let Some(from) = found {
            // Grown back over what was to be added, and forward as far as both go.
            let back = (1..=at.saturating_sub(added_from).min(from))
                .take_while(|&k| base[from - k] == new[at - k])
                .last()
                .unwrap_or(0);
            let (from, start) = (from - back, at - back);
            let len = base[from..].iter().zip(&new[start..]).take_while(|(a, b)| a == b).count();
            add(&mut out, &new[added_from..start]);
            out.push(COPY);
            varint(&mut out, from as u64);
            varint(&mut out, len as u64);
            at = start + len;
            added_from = at;
            hash = (at + WINDOW <= new.len()).then(|| window_hash(&new[at..at + WINDOW]));
            continue;
        }
        // Slid a byte on.
        hash = (at + WINDOW < new.len()).then(|| {
            h.wrapping_sub(u64::from(new[at]).wrapping_mul(high))
                .wrapping_mul(PRIME)
                .wrapping_add(u64::from(new[at + WINDOW]))
        });
        at += 1;
    }
    add(&mut out, &new[added_from..]);
    out
}

/// `base` with a bytes delta applied (`bytes_of`).
///
/// # Errors
/// It is no delta, or names bytes `base` has not, or makes a file of another length than it says.
pub fn apply_bytes(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let rest = delta.strip_prefix(MAGIC).context("no bytes delta of this format (`pe2 delta 1`)")?;
    let mut at = 0;
    let len = usize::try_from(read_varint(rest, &mut at)?)?;
    let mut out = Vec::with_capacity(len);
    while at < rest.len() {
        let op = rest[at];
        at += 1;
        match op {
            COPY => {
                let from = usize::try_from(read_varint(rest, &mut at)?)?;
                let n = usize::try_from(read_varint(rest, &mut at)?)?;
                let run = base.get(from..from.checked_add(n).context("a run past the end")?);
                out.extend_from_slice(run.context("a run the base has not")?);
            }
            ADD => {
                let n = usize::try_from(read_varint(rest, &mut at)?)?;
                let run = rest.get(at..at.checked_add(n).context("bytes past the end")?);
                out.extend_from_slice(run.context("a delta that ends early")?);
                at += n;
            }
            other => bail!("no operation of a delta is {other}"),
        }
    }
    ensure!(out.len() == len, "a delta that makes {} bytes, where it says {len}", out.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// Bytes of a seed, as noise.
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.max(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn a_bytes_delta_gives_the_new_bytes_back_and_follows_the_edit() {
        for seed in 1..40u32 {
            let base = noise(20_000 + seed as usize * 37, seed);
            let mut new = base.clone();
            // Edits in place, an insertion, a deletion, and bytes at both ends.
            let r = noise(64, seed + 100);
            let at = usize::from(r[0]) * 50 % new.len();
            new[at..at + 10].copy_from_slice(&r[..10]);
            new.splice(at / 2..at / 2, r[10..40].iter().copied());
            new.drain(at / 3..at / 3 + 25);
            new.splice(0..0, r[40..50].iter().copied());
            new.extend_from_slice(&r[50..]);
            let delta = super::bytes_of(&base, &new);
            assert_eq!(super::apply_bytes(&base, &delta).unwrap(), new, "seed {seed}");
            assert!(delta.len() < 400, "seed {seed}: {} bytes for an edit of a few", delta.len());
        }
        // Nothing alike: what is new is added whole; empty either way.
        for (base, new) in [(noise(100, 1), noise(100, 2)), (vec![], noise(50, 3)), (noise(50, 4), vec![])] {
            assert_eq!(super::apply_bytes(&base, &super::bytes_of(&base, &new)).unwrap(), new);
        }
        assert!(super::apply_bytes(b"", b"no delta").is_err());
    }

    #[test]
    fn a_lines_delta_gives_the_new_text_back() {
        let cases = [
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nb\nc", "x\na\nb\nc\ny"),
            ("", "only\n"),
            ("only\n", ""),
            ("a\r\nb\r\n", "a\r\n@@ 0 0 0\r\nb\r\n"),
        ];
        for (base, new) in cases {
            let delta = super::lines_of(base, new);
            assert_eq!(super::apply_lines(base, &delta).unwrap(), new, "{base:?} -> {new:?}: {delta:?}");
        }
        assert!(super::apply_lines("a\n", "@@ 5 1 0\n").is_err());
    }

    #[test]
    fn a_fields_delta_is_kept_only_where_it_gives_the_file_back() {
        let base = "# a node\nsize = 2 # its size\n\n[[stack]]\nid = \"a\"\nop = \"x\"\n";
        let new = "# a node\nsize = 3 # its size\n\n[[stack]]\nid = \"a\"\nop = \"y\"\n";
        let sets = super::fields(base, new).unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(super::apply_fields(base, &sets).unwrap(), new);
        // Laid out otherwise, or no TOML: none - what lines say.
        assert!(super::fields(base, "size = 3\n\n[[stack]]\nid = \"a\"\nop = \"y\"\n").is_none());
        assert!(super::fields("a = [", "a = 1\n").is_none());
    }
}
