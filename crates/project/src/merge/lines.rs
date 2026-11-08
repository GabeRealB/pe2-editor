//! Three-way merges of text by line (diff3): what `mine` changed of `base`, made over `theirs`, the version that
//! changed below. The lines `base` shares with both, in order, are the stable ones; between two, each side either left
//! `base` alone, changed it the same way as the other, or changed it otherwise - a conflict, where mine is kept.

use std::fmt::Write as _;

use similar::{Algorithm, DiffOp, capture_diff_slices};

use super::Conflict;

/// For each line of `base`, the line of `other` it is, where the diff keeps it.
fn matches(base: &[&str], other: &[&str]) -> Vec<Option<usize>> {
    let mut at = vec![None; base.len()];
    for op in capture_diff_slices(Algorithm::Myers, base, other) {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for i in 0..len {
                at[old_index + i] = Some(new_index + i);
            }
        }
    }
    at
}

/// Lines with their endings, so that joining them gives the text back.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A few lines of a run, for a person to recognize it; none for an empty run.
fn excerpt(run: &[&str]) -> Option<String> {
    const SHOWN: usize = 3;
    if run.is_empty() {
        return None;
    }
    let mut text: String =
        run.iter().take(SHOWN).map(|l| l.trim_end_matches(['\n', '\r'])).collect::<Vec<_>>().join("\n");
    if run.len() > SHOWN {
        let _ = write!(text, "\n… {} more lines", run.len() - SHOWN);
    }
    Some(text)
}

/// Merge `mine` over `theirs`, both changed from `base`: the text, and where the two changed the same lines otherwise.
#[must_use]
pub fn merge(base: &str, theirs: &str, mine: &str) -> (String, Vec<Conflict>) {
    let (base, theirs, mine) = (lines(base), lines(theirs), lines(mine));
    let (in_mine, in_theirs) = (matches(&base, &mine), matches(&base, &theirs));
    let mut out: Vec<&str> = Vec::with_capacity(mine.len().max(theirs.len()));
    let mut conflicts = Vec::new();
    let (mut b, mut m, mut t) = (0, 0, 0);
    while b < base.len() || m < mine.len() || t < theirs.len() {
        // A line of base both keep where they are now: taken, and on.
        if b < base.len() && in_mine[b] == Some(m) && in_theirs[b] == Some(t) {
            out.push(base[b]);
            (b, m, t) = (b + 1, m + 1, t + 1);
            continue;
        }
        // The next stable line, or the ends: the run before it is what each side made of base's run.
        let stable = (b..base.len()).find_map(|j| {
            let (jm, jt) = (in_mine[j]?, in_theirs[j]?);
            (jm >= m && jt >= t).then_some((j, jm, jt))
        });
        let (nb, nm, nt) = stable.unwrap_or((base.len(), mine.len(), theirs.len()));
        let (was, ours, other) = (&base[b..nb], &mine[m..nm], &theirs[t..nt]);
        let taken = if ours == was {
            other
        } else if other == was || ours == other {
            ours
        } else {
            let line = out.len() + 1;
            conflicts.push(Conflict {
                at: Some(if ours.is_empty() {
                    format!("line {line} (removed)")
                } else {
                    format!("lines {line}-{}", line + ours.len() - 1)
                }),
                mine: excerpt(ours),
                base: excerpt(was),
                theirs: excerpt(other),
            });
            ours
        };
        out.extend_from_slice(taken);
        (b, m, t) = (nb, nm, nt);
    }
    (out.concat(), conflicts)
}

#[cfg(test)]
mod tests {
    use super::merge;

    const BASE: &str = "a\nb\nc\nd\ne\n";

    #[test]
    fn one_side_changed_is_taken() {
        assert_eq!(merge(BASE, BASE, "a\nB\nc\nd\ne\n"), ("a\nB\nc\nd\ne\n".into(), vec![]));
        assert_eq!(merge(BASE, "a\nb\nc\nD\ne\n", BASE), ("a\nb\nc\nD\ne\n".into(), vec![]));
    }

    #[test]
    fn changes_apart_are_both_kept() {
        let (merged, conflicts) = merge(BASE, "a\nb\nc\nd\nE\nf\n", "x\na\nB\nc\nd\ne\n");
        assert_eq!(merged, "x\na\nB\nc\nd\nE\nf\n");
        assert_eq!(conflicts, vec![]);
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let (merged, conflicts) = merge(BASE, "a\nB\nc\nd\n", "a\nB\nc\nd\n");
        assert_eq!(merged, "a\nB\nc\nd\n");
        assert_eq!(conflicts, vec![]);
    }

    #[test]
    fn lines_changed_otherwise_conflict_and_mine_is_kept() {
        let (merged, conflicts) = merge(BASE, "a\nb\nC2\nd\ne\n", "a\nb\nC1\nd\ne\n");
        assert_eq!(merged, "a\nb\nC1\nd\ne\n");
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.at.as_deref(), Some("lines 3-3"));
        assert_eq!((c.mine.as_deref(), c.base.as_deref(), c.theirs.as_deref()), (Some("C1"), Some("c"), Some("C2")));
    }

    #[test]
    fn a_line_removed_on_one_side_and_changed_on_the_other_conflicts() {
        let (merged, conflicts) = merge(BASE, "a\nb\nC\nd\ne\n", "a\nb\nd\ne\n");
        assert_eq!(merged, "a\nb\nd\ne\n");
        assert_eq!(conflicts[0].at.as_deref(), Some("line 3 (removed)"));
    }

    #[test]
    fn empty_and_unterminated_texts_merge() {
        assert_eq!(merge("", "", "x"), ("x".into(), vec![]));
        // The last line, unterminated in base and mine, ended and followed on theirs: theirs's change is taken.
        assert_eq!(merge("a\nb", "a\nb\nc", "z\na\nb"), ("z\na\nb\nc".into(), vec![]));
    }
}
