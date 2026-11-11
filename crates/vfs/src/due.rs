//! What was done through the folder and is due to be the project's, kept where the project's processes share it
//! (`project::shared`'s space): whoever serves the folder says each thing due, in the order it is, and whoever
//! holds the project takes them from there - asked to look, or as it takes the project - makes each an edit, and
//! says what became of it in the same commit that says what is served after it. So what a program did is the
//! project's, and known to be, the moment the folder shows it so; and what is due stays due, whoever holds the
//! project or nobody, until someone takes it.
//!
//! Two trees of the space: what is due by its number, and what became of what was taken. A number is given once.
//! Who says something due may take it back while it is still due (`settle`): taken meanwhile, it is answered.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use sfs::space::{Pages, Space, Txn};

use crate::ops::Op;

/// The tree of what is due: a number, in the order things came due, to the edit it is.
pub const DUE: usize = 9;
/// The tree of what became of what was taken: the number of what was due to the edit's label, or why it was not
/// made.
pub const ANSWERS: usize = 10;

/// What became of something due: the label of the edit made of it, or why none was.
pub type Answer = Result<String, String>;

/// The next number to give: from when this process first gave one, so that none is given again by a process after
/// it.
static NEXT: AtomicU64 = AtomicU64::new(0);

fn number() -> u64 {
    let started = || {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX / 2).max(1)
    };
    let _ = NEXT.compare_exchange(0, started(), Ordering::Relaxed, Ordering::Relaxed);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn encoded<T: serde::Serialize>(it: &T) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    project::wire::send(&mut bytes, it)?;
    Ok(bytes)
}

fn decoded<T: serde::de::DeserializeOwned>(mut bytes: &[u8]) -> io::Result<T> {
    project::wire::receive(&mut bytes)?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "nothing is said"))
}

/// Something due, as the tree keeps it: the edit, and - a move whoever serves the folder made in what is served
/// already - what that move is known by there (`project::shared::moved`), for whoever takes it to say in its
/// commit that the project has it (`unmoved`).
#[derive(serde::Serialize, serde::Deserialize)]
struct Due {
    op: Op,
    moved: Option<u64>,
}

/// Say these due, in this order, after all that is due already: their numbers.
///
/// # Errors
/// The space cannot be written.
pub fn put(space: &Space, ops: &[Op]) -> io::Result<Vec<u64>> {
    let mut txn = space.write()?;
    let numbers = ops.iter().map(|op| put_in(&mut txn, op, None)).collect::<io::Result<Vec<u64>>>()?;
    txn.commit()?;
    Ok(numbers)
}

/// Say one thing due as part of a commit - the one that makes it so in what is served, where whoever serves the
/// folder did (`moved`: the move it is, as said there): its number.
///
/// # Errors
/// The space cannot be written.
pub fn put_in(txn: &mut Txn<'_>, op: &Op, moved: Option<u64>) -> io::Result<u64> {
    let number = number();
    sfs::tree::put(txn, DUE, &number.to_be_bytes(), &encoded(&Due { op: op.clone(), moved })?)?;
    Ok(number)
}

/// Say something that is due as it is now, under the number it is due by - its place among what is due kept:
/// what a written file holds, said long ago and written since or moved on in the log. Whether it was still due;
/// taken meanwhile, it is as it was said.
///
/// # Errors
/// The space cannot be written.
pub fn replace(space: &Space, number: u64, op: &Op) -> io::Result<bool> {
    let mut txn = space.write()?;
    let key = number.to_be_bytes();
    let Some(was) = sfs::tree::get(&txn, DUE, &key)? else { return Ok(false) };
    let moved = decoded::<Due>(&was)?.moved;
    sfs::tree::put(&mut txn, DUE, &key, &encoded(&Due { op: op.clone(), moved })?)?;
    txn.commit()?;
    Ok(true)
}

/// All that is due, in the order it came due.
///
/// # Errors
/// The space cannot be read, or holds what is no edit.
pub fn due(pages: &impl Pages) -> io::Result<Vec<(u64, Op)>> {
    let mut found = Vec::new();
    let mut failed = None;
    sfs::tree::scan(pages, DUE, &[], &mut |key, value| {
        let number = <[u8; 8]>::try_from(key).map(u64::from_be_bytes);
        match (number, decoded::<Due>(value)) {
            (Ok(number), Ok(due)) => found.push((number, due.op)),
            (_, Err(e)) => failed = Some(e),
            (Err(_), _) => failed = Some(io::Error::new(io::ErrorKind::InvalidData, "a number of another length")),
        }
        failed.is_none()
    })?;
    failed.map_or(Ok(found), Err)
}

/// Whether anything is due.
#[must_use]
pub fn any(pages: &impl Pages) -> bool {
    pages.root(DUE) != 0 && {
        let mut any = false;
        let _ = sfs::tree::scan(pages, DUE, &[], &mut |_, _| {
            any = true;
            false
        });
        any
    }
}

/// Say what became of things that were due, as part of a commit - the one saying what is served after them: each
/// is due no more and answered, where it is still due. One taken back meanwhile is answered to nobody.
///
/// # Errors
/// The space cannot be written.
pub fn answer(txn: &mut Txn<'_>, answers: &[(u64, Answer)]) -> io::Result<()> {
    for (number, answer) in answers {
        let key = number.to_be_bytes();
        let Some(due) = sfs::tree::get(&*txn, DUE, &key)? else { continue };
        sfs::tree::remove(txn, DUE, &key)?;
        sfs::tree::put(txn, ANSWERS, &key, &encoded(answer)?)?;
        // A move the project has now: what it says of paths from here on is said with the move made. One it
        // refused stays one its paths are read through, until whoever serves the folder has put it back.
        if let (Ok(_), Some(moved)) = (answer, decoded::<Due>(&due)?.moved) {
            project::shared::unmoved(txn, moved)?;
        }
    }
    Ok(())
}

/// Say what became of things that were due, in a commit of its own: where nothing is served, or nothing of what
/// is served changed.
///
/// # Errors
/// The space cannot be written.
pub fn answer_now(space: &Space, answers: &[(u64, Answer)]) -> io::Result<()> {
    if answers.is_empty() {
        return Ok(());
    }
    let mut txn = space.write()?;
    answer(&mut txn, answers)?;
    Ok(txn.commit()?)
}

/// What became of things said due, for who said them: each one's answer, taken out - none where it is still due,
/// and then, where `take_back`, due no more.
///
/// # Errors
/// The space cannot be written.
pub fn settle(space: &Space, numbers: &[u64], take_back: bool) -> io::Result<Vec<Option<Answer>>> {
    let mut txn = space.write()?;
    let mut became = Vec::with_capacity(numbers.len());
    let mut changed = false;
    for number in numbers {
        let key = number.to_be_bytes();
        if let Some(answer) = sfs::tree::get(&txn, ANSWERS, &key)? {
            sfs::tree::remove(&mut txn, ANSWERS, &key)?;
            changed = true;
            became.push(Some(decoded::<Answer>(&answer)?));
        } else {
            changed |= take_back && sfs::tree::remove(&mut txn, DUE, &key)?;
            became.push(None);
        }
    }
    if changed {
        txn.commit()?;
    }
    Ok(became)
}

/// Forget all that is due and all that was answered: what whoever serves the folder does as it starts, since what
/// a serving before it said due it says again itself, from the log.
///
/// # Errors
/// The space cannot be written.
pub fn clear(space: &Space) -> io::Result<()> {
    let mut txn = space.write()?;
    let mut keys = Vec::new();
    for tree in [DUE, ANSWERS] {
        keys.clear();
        sfs::tree::scan(&txn, tree, &[], &mut |key, _| {
            keys.push(key.to_vec());
            true
        })?;
        for key in &keys {
            sfs::tree::remove(&mut txn, tree, key)?;
        }
    }
    Ok(txn.commit()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str) -> Op {
        Op::Folder { path: path.to_string() }
    }

    #[test]
    fn what_is_due_is_taken_in_its_order_and_answered_to_who_said_it() {
        let space = Space::private().unwrap();
        let first = put(&space, &[folder("a"), folder("a/b")]).unwrap();
        let second = put(&space, &[Op::Rename { from: "a".into(), to: "c".into() }]).unwrap();
        assert!(any(&space.read().unwrap()));
        let found = due(&space.read().unwrap()).unwrap();
        let numbers: Vec<u64> = found.iter().map(|(number, _)| *number).collect();
        assert_eq!(numbers, [first[0], first[1], second[0]], "in the order it came due");
        assert_eq!(found[1].1, folder("a/b"));
        // Nothing is answered yet, and nothing taken back for asking.
        assert_eq!(settle(&space, &first, false).unwrap(), [None, None]);
        assert_eq!(due(&space.read().unwrap()).unwrap().len(), 3);
        // The holder answers two of them with its commit; they are due no more.
        let mut txn = space.write().unwrap();
        answer(&mut txn, &[(first[0], Ok("Create folder a".into())), (first[1], Err("not here".into()))]).unwrap();
        txn.commit().unwrap();
        assert_eq!(due(&space.read().unwrap()).unwrap().len(), 1);
        assert_eq!(settle(&space, &first, true).unwrap(), [Some(Ok("Create folder a".into())), Some(Err("not here".into()))]);
        // Taken out: asked again, there is nothing to say of them.
        assert_eq!(settle(&space, &first, true).unwrap(), [None, None]);
        // The third is taken back while still due: answered after that, it is answered to nobody.
        assert_eq!(settle(&space, &second, true).unwrap(), [None]);
        assert!(!any(&space.read().unwrap()));
        answer_now(&space, &[(second[0], Ok("Rename a".into()))]).unwrap();
        assert_eq!(settle(&space, &second, false).unwrap(), [None]);
    }

    #[test]
    fn an_edit_follows_those_naming_a_path_at_in_or_above_its_own() {
        let earlier = vec!["a/b".to_string(), "c.txt".to_string()];
        assert!(folder("a/b/inside").follows(&earlier), "in a folder they made");
        assert!(folder("a").follows(&earlier), "above one");
        assert!(Op::Rename { from: "x".into(), to: "c.txt".into() }.follows(&earlier), "onto a file they wrote");
        assert!(Op::Remove { path: "a/b".into() }.follows(&earlier));
        assert!(!folder("a/bc").follows(&earlier), "beside it, whatever its name starts with");
        assert!(!folder("d").follows(&earlier));
        assert!(!Op::Undo.follows(&earlier));
    }

    #[test]
    fn a_serving_that_starts_forgets_what_the_one_before_said() {
        let space = Space::private().unwrap();
        let said = put(&space, &[folder("a"), folder("b")]).unwrap();
        answer_now(&space, &[(said[0], Ok("Create folder a".into()))]).unwrap();
        clear(&space).unwrap();
        assert!(!any(&space.read().unwrap()));
        assert_eq!(settle(&space, &said, false).unwrap(), [None, None]);
        // And a written file's content, long as its ranges are, is said as it is.
        let ranges = (0..4000u64).map(|at| (at * 4096, at * 9000, 4096)).collect::<Vec<_>>();
        let written = crate::write::Written::Logged { log: "/somewhere/sfs".into(), len: 4000 * 4096, ranges };
        let op = Op::Write { path: "big.bin".into(), from: written };
        put(&space, std::slice::from_ref(&op)).unwrap();
        assert_eq!(due(&space.read().unwrap()).unwrap()[0].1, op);
    }
}
