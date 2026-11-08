//! Three-way merges of a file: what a layer made of a version below (`base` → `mine`), made over what is below now
//! (`theirs`). One way for every file, chosen by what it is, never by what node it is of: TOML by field
//! (`fields`), other text by line (`lines`), anything else not at all - a binary file changed below since is a
//! conflict. Where both sides changed the same thing otherwise, mine is kept, and the conflict is said.

pub mod fields;
pub mod lines;

/// Where both sides changed the same thing otherwise: where (a field, lines; none for a whole file), and what each
/// side has there (none: absent), as a person reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub at: Option<String>,
    pub mine: Option<String>,
    pub base: Option<String>,
    pub theirs: Option<String>,
}

/// A merge's result: the file, and its conflicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub bytes: Vec<u8>,
    pub conflicts: Vec<Conflict>,
}

/// Merge a file named `name`: `mine`, made of `base`, over `theirs`, which `base` became below.
#[must_use]
pub fn merge(name: &str, base: &[u8], theirs: &[u8], mine: &[u8]) -> Merged {
    let clean = |bytes: &[u8]| Merged { bytes: bytes.to_vec(), conflicts: vec![] };
    // One side alone changed it, or both alike.
    if theirs == base || theirs == mine {
        return clean(mine);
    }
    if mine == base {
        return clean(theirs);
    }
    let texts = (std::str::from_utf8(base), std::str::from_utf8(theirs), std::str::from_utf8(mine));
    if let (Ok(base), Ok(theirs), Ok(mine)) = texts {
        let toml = std::path::Path::new(name).extension().is_some_and(|e| e == "toml");
        if toml && let Some((merged, conflicts)) = fields::merge(base, theirs, mine) {
            return Merged { bytes: merged.into_bytes(), conflicts };
        }
        let (merged, conflicts) = lines::merge(base, theirs, mine);
        return Merged { bytes: merged.into_bytes(), conflicts };
    }
    // Bytes that are no text: either side whole, the layer's kept.
    let conflict = Conflict {
        at: None,
        mine: Some("its own".into()),
        base: Some("the version it was made from".into()),
        theirs: Some("another version".into()),
    };
    Merged { bytes: mine.to_vec(), conflicts: vec![conflict] }
}

#[cfg(test)]
mod tests {
    use super::merge;

    #[test]
    fn a_file_is_merged_as_what_it_is() {
        // TOML by field: the same lines changed otherwise would conflict, the fields do not.
        let merged = merge("a.toml", b"a = 1\nb = 2\n", b"a = 1\nb = 3\n", b"a = 0\nb = 2\n");
        assert_eq!((merged.bytes.as_slice(), merged.conflicts.len()), (&b"a = 0\nb = 3\n"[..], 0));
        // Other text by line (lines changed apart; adjacent ones are one run, as diff3 has them).
        let merged = merge("a.evs", b"x\nm\ny\n", b"x\nm\nY\n", b"X\nm\ny\n");
        assert_eq!((merged.bytes.as_slice(), merged.conflicts.len()), (&b"X\nm\nY\n"[..], 0));
        // Bytes changed on both sides: mine, and a conflict; changed on one side alone: that side.
        let merged = merge("a.bin", &[0, 255], &[1, 255], &[2, 255]);
        assert_eq!((merged.bytes.as_slice(), merged.conflicts.len()), (&[2u8, 255][..], 1));
        assert_eq!(merge("a.bin", &[0, 255], &[1, 255], &[0, 255]).bytes, [1, 255]);
        // TOML that does not parse is text.
        let merged = merge("a.toml", b"a = [\nm\nb\n", b"a = [\nm\nB\n", b"A = [\nm\nb\n");
        assert_eq!(merged.bytes, b"A = [\nm\nB\n");
    }
}
