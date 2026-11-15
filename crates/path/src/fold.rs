//! What names are compared by: their fold, under which names any system the editor runs on takes as one are equal.
//!
//! A text folds to its canonical decomposition, each character then folded (`build.rs`: the full case folding of its
//! simple uppercase), decomposed again - the canonical caseless match of the Unicode standard, joined with what NTFS
//! takes as alike. Names equal under it are one name to the project's tree: what is written is kept as it is written,
//! and shown and put on disk so.
//!
//! An ASCII text folds to its ASCII lowercase, compared byte by byte as it is read: nothing is made of it. Any other is
//! folded into a text made for the comparison.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::hash::Hasher;

include!(concat!(env!("OUT_DIR"), "/fold.rs"));

/// What a character folds to, where the fold changes it.
fn folds_to(c: char) -> Option<&'static str> {
    let at = FOLDS.binary_search_by_key(&u32::from(c), |(code, _)| *code).ok()?;
    Some(FOLDS[at].1)
}

/// A text's fold.
#[must_use]
pub fn folded(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return if text.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
        };
    }
    let normalizer = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    let decomposed = normalizer.normalize(text);
    let mut mapped = String::with_capacity(decomposed.len());
    for c in decomposed.chars() {
        match folds_to(c) {
            Some(to) => mapped.push_str(to),
            None => mapped.push(c),
        }
    }
    Cow::Owned(normalizer.normalize(&mapped).into_owned())
}

/// Whether two texts fold alike.
pub(crate) fn same(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    folded(a) == folded(b)
}

/// How two texts are ordered by their folds.
pub(crate) fn order(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.bytes().map(|b| b.to_ascii_lowercase()).cmp(b.bytes().map(|b| b.to_ascii_lowercase()));
    }
    folded(a).cmp(&folded(b))
}

/// A text hashed as its fold: its bytes in blocks of one size, whichever way they are made, so that a hasher that
/// hashes each write apart (not every one streams) gives two of one fold one hash.
pub(crate) fn hash(text: &str, state: &mut impl Hasher) {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    let mut filled = 0;
    let mut put = |byte: u8, state: &mut dyn Hasher| {
        block[filled] = byte;
        filled += 1;
        if filled == BLOCK {
            state.write(&block);
            filled = 0;
        }
    };
    if text.is_ascii() {
        for byte in text.bytes() {
            put(byte.to_ascii_lowercase(), state);
        }
    } else {
        for byte in folded(text).bytes() {
            put(byte, state);
        }
    }
    state.write(&block[..filled]);
    state.write_u8(0xff);
}

#[cfg(test)]
mod tests {
    use super::{folded, hash, order, same};
    use std::cmp::Ordering;
    use std::hash::{DefaultHasher, Hasher};

    fn hashed(text: &str) -> u64 {
        let mut state = DefaultHasher::new();
        hash(text, &mut state);
        state.finish()
    }

    #[test]
    fn names_any_system_takes_as_one_fold_alike() {
        for (a, b) in [
            ("notes", "NOTES"),
            ("Straße", "STRASSE"),
            ("ΟΔΟΣ", "οδος"),
            ("ΟΔΟΣ", "οδοσ"),
            // NTFS uppercases `ı` to `I`: one name there.
            ("fıle", "FILE"),
            ("İ", "i\u{307}"),
            ("café", "cafe\u{301}"),
            ("\u{212A}elvin", "kelvin"),
        ] {
            assert!(same(a, b), "{a} {b}");
            assert_eq!(order(a, b), Ordering::Equal, "{a} {b}");
            assert_eq!(hashed(a), hashed(b), "{a} {b}");
        }
        // What only a compatibility mapping takes as alike is not: `ǅ` is no `dž`, `ﬁ` folds to `fi` as case folding has it.
        assert!(!same("\u{1C5}", "d\u{17E}"));
        assert!(same("\u{FB01}le", "file"));
        assert!(!same("a", "b") && order("a", "B") == Ordering::Less);
    }

    #[test]
    fn an_ascii_name_folds_to_its_lowercase_and_hashes_as_any_would() {
        assert_eq!(folded("Node.TOML"), "node.toml");
        assert!(matches!(folded("node.toml"), std::borrow::Cow::Borrowed(_)));
        // Longer than a block, ASCII or not: one fold, one hash.
        let long = "A".repeat(70);
        assert_eq!(hashed(&long), hashed(&"a".repeat(70)));
        assert_eq!(hashed(&format!("{long}ß")), hashed(&format!("{}SS", "a".repeat(70))));
        assert_eq!(super::UNICODE_VERSION, "18.0.0");
    }
}
