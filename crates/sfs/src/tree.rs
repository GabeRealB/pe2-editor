//! Trees in the space's pages: keys in order, each with a value, changed by copying what is changed
//! (`space::Txn::own`) - so a reader at an earlier commit goes on reading the tree as it was. A page is a leaf
//! (keys with their values), a branch (keys that part its children), or the rest of a value too long for a leaf.
//!
//! A page after its head (`space::PAGE_HEAD`): how many cells it holds (2), where the lowest of them begins (2), a
//! branch's first child (8); then where each cell is (2 each), in the order of their keys; the cells fill the page
//! from its end. A cell is put into a page of the transaction's own where the room between that list and the
//! cells takes it, and taken out of the list alone; what that leaves unused between cells is taken back when the
//! page is next written whole - where a cell no longer fits that room. A
//! leaf's cell is its key's length (2), its value's (4), the key, and the value - or, for one that would make the
//! cell longer than a third of a page (`CELL`), the page its bytes go on in (8). A branch's cell is its key's length (2), a child (8), and the key: the child holds the keys
//! from that one on, the first child those before all of them.
//!
//! A page from which keys were taken is not joined with the one beside it: it goes when its last key does, and a
//! branch left with one child gives way to that child.
//!
//! A cell that is not within its page is said as any page that is not what it has to be (`Pages::damaged`).

use crate::space::{Error, PAGE, PAGE_HEAD, Pages, Result, Txn};

const LEAF: u8 = 1;
const BRANCH: u8 = 2;
const MORE: u8 = 3;

/// Where a page's cells are listed from.
const SLOTS: usize = PAGE_HEAD + 12;
/// The longest key: a folder's number and the longest name a system has (255 units of UTF-16, as UTF-8) fit.
pub const KEY: usize = 800;
/// The longest cell a leaf holds - where it is, its lengths, its key and its value: a value that would make it
/// longer goes on in pages of its own. Three such cells fit a page, so a page that no longer fits always parts
/// into two that do (`part`); with a short key a value of 1.3 KiB is held in its leaf, with the longest one of half
/// a kibibyte.
const CELL: usize = (PAGE - SLOTS) / 3;
/// How far a page is filled where a tree is loaded whole (`load`): what is put into it after finds room.
const FILLED: usize = PAGE - PAGE / 10;
/// What a page of a long value holds of it, after its head, the next page (8) and how much (4).
const MORE_DATA: usize = PAGE - PAGE_HEAD - 12;
/// Marks a value's length as that of a long one.
const LONG: u32 = 1 << 31;
/// No tree is as deep as this: one that seems to be leads back into itself.
const DEEP: usize = 64;

fn u16_at(page: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([page[at], page[at + 1]]))
}

fn u32_at(page: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(page[at..at + 4].try_into().unwrap_or_default())
}

fn u64_at(page: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(page[at..at + 8].try_into().unwrap_or_default())
}

/// How many cells a page holds: no more than could be listed in it.
fn count(page: &[u8]) -> usize {
    u16_at(page, PAGE_HEAD).min((PAGE - SLOTS) / 2)
}

/// Where the lowest of a page's cells begins: the room for another ends there.
fn begin(page: &[u8]) -> usize {
    match u16_at(page, PAGE_HEAD + 2) {
        0 => PAGE,
        begin => begin,
    }
}

fn set_u16(page: &mut [u8], at: usize, value: usize) {
    page[at..at + 2].copy_from_slice(&u16::try_from(value).unwrap_or(0).to_le_bytes());
}

/// Put a cell - its parts one after another - into a page at a place among its cells, where the room between its
/// list and its cells takes it: whether it did.
fn insert(page: &mut [u8], i: usize, cell: &[&[u8]]) -> bool {
    let (cells, begin) = (count(page), begin(page));
    let len: usize = cell.iter().map(|part| part.len()).sum();
    if i > cells || begin > PAGE || begin < SLOTS + 2 * (cells + 1) + len {
        return false;
    }
    let at = begin - len;
    let mut to = at;
    for part in cell {
        page[to..to + part.len()].copy_from_slice(part);
        to += part.len();
    }
    page.copy_within(SLOTS + i * 2..SLOTS + cells * 2, SLOTS + i * 2 + 2);
    set_u16(page, SLOTS + i * 2, at);
    set_u16(page, PAGE_HEAD, cells + 1);
    set_u16(page, PAGE_HEAD + 2, at);
    true
}

/// Take a cell out of a page's list: its bytes stay, room the next writing of the page whole takes back.
fn take_out(page: &mut [u8], i: usize) {
    let cells = count(page);
    if i < cells {
        page.copy_within(SLOTS + (i + 1) * 2..SLOTS + cells * 2, SLOTS + i * 2);
        set_u16(page, SLOTS + (cells - 1) * 2, 0);
        set_u16(page, PAGE_HEAD, cells - 1);
    }
}

/// A page whose cells are not within it, or that leads where no tree does: said, as any page that is not what it
/// has to be.
fn torn(pages: &impl Pages, page: &[u8]) -> Error {
    pages.damaged(format!("page {} of a tree holds what is not within it", u32_at(page, 4)))
}

/// Where a cell is, by its place among the page's: one of at least this length, within the page.
fn cell(page: &[u8], i: usize, least: usize) -> Option<usize> {
    let at = u16_at(page, SLOTS + i * 2);
    (i < count(page) && at >= SLOTS && at + least <= PAGE).then_some(at)
}

enum Value<'a> {
    Here(&'a [u8]),
    Long { len: u32, first: u64 },
}

/// A leaf's cell: its key, and its value - held there, or the length and first page of a long one.
fn leaf_cell(page: &[u8], i: usize) -> Option<(&[u8], Value<'_>)> {
    let at = cell(page, i, 6)?;
    let value_len = u32_at(page, at + 2);
    let from = at + 6 + u16_at(page, at);
    let key = page.get(at + 6..from)?;
    if value_len & LONG != 0 {
        let first = u64::from_le_bytes(page.get(from..from + 8)?.try_into().ok()?);
        Some((key, Value::Long { len: value_len & !LONG, first }))
    } else {
        Some((key, Value::Here(page.get(from..from + usize::try_from(value_len).ok()?)?)))
    }
}

/// A branch's cell: its key and its child.
fn branch_cell(page: &[u8], i: usize) -> Option<(&[u8], u64)> {
    let at = cell(page, i, 10)?;
    Some((page.get(at + 10..at + 10 + u16_at(page, at))?, u64_at(page, at + 2)))
}

/// The child of a branch that holds a key - the last whose key is not after it, or the first - and how many of the
/// branch's keys are not after it.
fn child_for(page: &[u8], key: &[u8]) -> Option<(usize, u64)> {
    let (mut low, mut high) = (0, count(page));
    while low < high {
        let mid = usize::midpoint(low, high);
        if branch_cell(page, mid)?.0 <= key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    if low == 0 { Some((0, u64_at(page, PAGE_HEAD + 4))) } else { Some((low, branch_cell(page, low - 1)?.1)) }
}

/// Where a key is in a leaf, or would be.
fn find(page: &[u8], key: &[u8]) -> Option<std::result::Result<usize, usize>> {
    let (mut low, mut high) = (0, count(page));
    while low < high {
        let mid = usize::midpoint(low, high);
        match leaf_cell(page, mid)?.0.cmp(key) {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => return Some(Ok(mid)),
        }
    }
    Some(Err(low))
}

/// A long value, read whole from its pages.
fn long(pages: &impl Pages, len: u32, mut at: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).unwrap_or(0);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let page = pages.page(at, MORE)?;
        let held = usize::try_from(u32_at(page, PAGE_HEAD + 8)).unwrap_or(usize::MAX);
        if held == 0 || held > MORE_DATA || out.len() + held > len {
            return Err(torn(pages, page));
        }
        out.extend_from_slice(&page[PAGE_HEAD + 12..PAGE_HEAD + 12 + held]);
        at = u64_at(page, PAGE_HEAD);
    }
    Ok(out)
}

/// The value of a key in a tree of the space; none where the tree has no such key.
///
/// # Errors
/// The space is damaged.
pub fn get(pages: &impl Pages, tree: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(get_in(pages, tree, key)?.map(std::borrow::Cow::into_owned))
}

/// The value of a key as `get` finds it, read where it is: one held in its leaf is lent from the page, for as
/// long as what reads the pages is held, and only a long one is put together.
///
/// # Errors
/// The space is damaged.
pub fn get_in<'p>(pages: &'p impl Pages, tree: usize, key: &[u8]) -> Result<Option<std::borrow::Cow<'p, [u8]>>> {
    use std::borrow::Cow;
    let mut at = pages.root(tree);
    if at == 0 {
        return Ok(None);
    }
    for _ in 0..DEEP {
        let page = pages.page_either(at, LEAF, BRANCH)?;
        if page[0] == BRANCH {
            at = child_for(page, key).ok_or_else(|| torn(pages, page))?.1;
            continue;
        }
        let Ok(i) = find(page, key).ok_or_else(|| torn(pages, page))? else { return Ok(None) };
        return match leaf_cell(page, i).ok_or_else(|| torn(pages, page))?.1 {
            Value::Here(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            Value::Long { len, first } => long(pages, len, first).map(|value| Some(Cow::Owned(value))),
        };
    }
    Err(pages.damaged("a tree deeper than any is: it leads back into itself".into()))
}

/// Every key from one on, in order, with its value, given to `each` until it says to stop.
///
/// # Errors
/// The space is damaged.
pub fn scan(pages: &impl Pages, tree: usize, from: &[u8], each: &mut dyn FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
    let root = pages.root(tree);
    if root != 0 {
        scan_page(pages, root, from, each, 0)?;
    }
    Ok(())
}

/// Every key that begins with `prefix`, in order, with its value, given to `each` until it says to stop.
///
/// # Errors
/// The space is damaged.
pub fn scan_prefix(pages: &impl Pages, tree: usize, prefix: &[u8], each: &mut dyn FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
    scan(pages, tree, prefix, &mut |key, value| key.starts_with(prefix) && each(key, value))
}

/// Scan a page and what is under it: whether to go on.
fn scan_page(pages: &impl Pages, at: u64, from: &[u8], each: &mut dyn FnMut(&[u8], &[u8]) -> bool, depth: usize) -> Result<bool> {
    let page = pages.page_either(at, LEAF, BRANCH)?;
    let torn = || torn(pages, page);
    if page[0] == LEAF {
        let first = find(page, from).ok_or_else(torn)?.unwrap_or_else(|at| at);
        for i in first..count(page) {
            let (key, value) = leaf_cell(page, i).ok_or_else(torn)?;
            let go_on = match value {
                Value::Here(bytes) => each(key, bytes),
                Value::Long { len, first } => each(key, &long(pages, len, first)?),
            };
            if !go_on {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if depth > DEEP {
        return Err(pages.damaged("a tree deeper than any is: it leads back into itself".into()));
    }
    // From the child that holds `from`, then every one after it.
    let (first, child) = child_for(page, from).ok_or_else(torn)?;
    if !scan_page(pages, child, from, each, depth + 1)? {
        return Ok(false);
    }
    for i in first..count(page) {
        if !scan_page(pages, branch_cell(page, i).ok_or_else(torn)?.1, from, each, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Where to part cells that no longer fit one page, by what each takes of it: the place that leaves the two sides
/// nearest to each other, both fitting a page. With `up`, the cell at that place goes to neither side (a branch's
/// middle key, which goes up). There is one, as no cell takes more than a third of a page.
fn part(sizes: &[usize], up: bool) -> usize {
    let total: usize = sizes.iter().sum();
    let mut left = 0;
    let mut best = (usize::MAX, 1);
    for at in 1..sizes.len().saturating_sub(usize::from(up)) {
        left += sizes[at - 1];
        let right = total - left - if up { sizes[at] } else { 0 };
        if SLOTS + left <= PAGE && SLOTS + right <= PAGE && left.abs_diff(right) < best.0 {
            best = (left.abs_diff(right), at);
        }
    }
    best.1
}

/// A value as a leaf holds it.
#[derive(Clone)]
enum Held {
    Here(Vec<u8>),
    Long { len: u32, first: u64 },
}

/// A leaf's cells, taken out of its page to be changed.
struct Leaf {
    cells: Vec<(Vec<u8>, Held)>,
}

impl Leaf {
    fn read(page: &[u8]) -> Option<Leaf> {
        let cells = (0..count(page)).map(|i| {
            let (key, value) = leaf_cell(page, i)?;
            let held = match value {
                Value::Here(bytes) => Held::Here(bytes.to_vec()),
                Value::Long { len, first } => Held::Long { len, first },
            };
            Some((key.to_vec(), held))
        });
        Some(Leaf { cells: cells.collect::<Option<_>>()? })
    }

    /// What a cell takes of a page: where it is (2), its lengths (6), its key, and its value or where that goes on.
    fn cell_size((key, held): &(Vec<u8>, Held)) -> usize {
        8 + key.len()
            + match held {
                Held::Here(bytes) => bytes.len(),
                Held::Long { .. } => 8,
            }
    }

    fn size(cells: &[(Vec<u8>, Held)]) -> usize {
        SLOTS + cells.iter().map(Leaf::cell_size).sum::<usize>()
    }

    fn write(page: &mut [u8], cells: &[(Vec<u8>, Held)]) {
        page[PAGE_HEAD..].fill(0);
        let mut end = PAGE;
        for (i, (key, held)) in cells.iter().enumerate() {
            let (value_len, value): (u32, &[u8]) = match held {
                Held::Here(bytes) => (u32::try_from(bytes.len()).unwrap_or(0), bytes),
                Held::Long { len, .. } => (*len | LONG, &[]),
            };
            let goes_on = match held {
                Held::Long { first, .. } => first.to_le_bytes().to_vec(),
                Held::Here(_) => Vec::new(),
            };
            end -= 6 + key.len() + value.len() + goes_on.len();
            page[end..end + 2].copy_from_slice(&u16::try_from(key.len()).unwrap_or(0).to_le_bytes());
            page[end + 2..end + 6].copy_from_slice(&value_len.to_le_bytes());
            page[end + 6..end + 6 + key.len()].copy_from_slice(key);
            let from = end + 6 + key.len();
            page[from..from + value.len()].copy_from_slice(value);
            page[from + value.len()..from + value.len() + goes_on.len()].copy_from_slice(&goes_on);
            set_u16(page, SLOTS + i * 2, end);
        }
        set_u16(page, PAGE_HEAD, cells.len());
        set_u16(page, PAGE_HEAD + 2, end);
    }
}

/// A branch's first child and cells, taken out of its page to be changed.
struct Branch {
    first: u64,
    cells: Vec<(Vec<u8>, u64)>,
}

impl Branch {
    fn read(page: &[u8]) -> Option<Branch> {
        let cells = (0..count(page)).map(|i| branch_cell(page, i).map(|(key, child)| (key.to_vec(), child)));
        Some(Branch { first: u64_at(page, PAGE_HEAD + 4), cells: cells.collect::<Option<_>>()? })
    }

    fn size(cells: &[(Vec<u8>, u64)]) -> usize {
        SLOTS + cells.iter().map(|(key, _)| 12 + key.len()).sum::<usize>()
    }

    fn write(page: &mut [u8], first: u64, cells: &[(Vec<u8>, u64)]) {
        page[PAGE_HEAD..].fill(0);
        page[PAGE_HEAD + 4..PAGE_HEAD + 12].copy_from_slice(&first.to_le_bytes());
        let mut end = PAGE;
        for (i, (key, child)) in cells.iter().enumerate() {
            end -= 10 + key.len();
            page[end..end + 2].copy_from_slice(&u16::try_from(key.len()).unwrap_or(0).to_le_bytes());
            page[end + 2..end + 10].copy_from_slice(&child.to_le_bytes());
            page[end + 10..end + 10 + key.len()].copy_from_slice(key);
            set_u16(page, SLOTS + i * 2, end);
        }
        set_u16(page, PAGE_HEAD, cells.len());
        set_u16(page, PAGE_HEAD + 2, end);
    }
}

/// Let go of the pages a long value goes on in.
fn let_go_long(txn: &mut Txn<'_>, len: u32, mut at: u64) -> Result<()> {
    let mut left = usize::try_from(len).unwrap_or(0);
    while left > 0 {
        let page = txn.page(at, MORE)?;
        let held = usize::try_from(u32_at(page, PAGE_HEAD + 8)).unwrap_or(usize::MAX);
        if held == 0 || held > left {
            return Err(torn(txn, page));
        }
        let next = u64_at(page, PAGE_HEAD);
        txn.let_go(at);
        left -= held;
        at = next;
    }
    Ok(())
}

/// A value as a leaf is to hold it under a key of this length: itself, or - where its cell would be too long for
/// a leaf - written to pages of its own.
fn held(txn: &mut Txn<'_>, key: usize, value: &[u8]) -> Result<Held> {
    if 8 + key + value.len() <= CELL {
        return Ok(Held::Here(value.to_vec()));
    }
    let Ok(len) = u32::try_from(value.len()) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a value longer than a tree holds").into());
    };
    if len & LONG != 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a value longer than a tree holds").into());
    }
    // Written from its end, so that each page says the one after it.
    let mut next = 0u64;
    for part in value.chunks(MORE_DATA).rev() {
        let at = txn.make(MORE)?;
        let page = txn.page_mut(at)?;
        page[PAGE_HEAD..PAGE_HEAD + 8].copy_from_slice(&next.to_le_bytes());
        page[PAGE_HEAD + 8..PAGE_HEAD + 12].copy_from_slice(&u32::try_from(part.len()).unwrap_or(0).to_le_bytes());
        page[PAGE_HEAD + 12..PAGE_HEAD + 12 + part.len()].copy_from_slice(part);
        next = at;
    }
    Ok(Held::Long { len, first: next })
}

/// What putting a key under a page came to: the page that holds it now, and - where it no longer fit - the key
/// from which a second page beside it holds the rest.
type Put = (u64, Option<(Vec<u8>, u64)>);

fn put_under(txn: &mut Txn<'_>, at: u64, key: &[u8], value: &[u8], depth: usize) -> Result<Put> {
    let page = txn.page_either(at, LEAF, BRANCH)?;
    if page[0] == LEAF {
        let found = find(page, key).ok_or_else(|| torn(txn, page))?;
        let was_long = match found {
            Ok(i) => match leaf_cell(page, i).ok_or_else(|| torn(txn, page))?.1 {
                Value::Long { len, first } => Some((len, first)),
                Value::Here(_) => None,
            },
            Err(_) => None,
        };
        let new = held(txn, key.len(), value)?;
        if let Some((len, first)) = was_long {
            let_go_long(txn, len, first)?;
        }
        let mine = txn.own(at, LEAF)?;
        let page = txn.page_mut(mine)?;
        // The cell it had goes; the new one is put where the room takes it.
        let i = match found {
            Ok(i) => {
                take_out(page, i);
                i
            }
            Err(i) => i,
        };
        let key_len = u16::try_from(key.len()).unwrap_or(0).to_le_bytes();
        let fit = match &new {
            Held::Here(bytes) => insert(page, i, &[&key_len, &u32::try_from(bytes.len()).unwrap_or(0).to_le_bytes(), key, bytes]),
            Held::Long { len, first } => insert(page, i, &[&key_len, &(*len | LONG).to_le_bytes(), key, &first.to_le_bytes()]),
        };
        if fit {
            return Ok((mine, None));
        }
        // Written whole: what was left unused between its cells is room again - or it parts in two.
        let Some(mut leaf) = Leaf::read(page) else {
            return Err(txn.damaged("a leaf of a tree holds what is not within it".into()));
        };
        leaf.cells.insert(i.min(leaf.cells.len()), (key.to_vec(), new));
        if Leaf::size(&leaf.cells) <= PAGE {
            Leaf::write(page, &leaf.cells);
            return Ok((mine, None));
        }
        // Parted where each side holds about as much.
        let sizes: Vec<usize> = leaf.cells.iter().map(Leaf::cell_size).collect();
        let right = leaf.cells.split_off(part(&sizes, false));
        Leaf::write(page, &leaf.cells);
        let beside = txn.make(LEAF)?;
        Leaf::write(txn.page_mut(beside)?, &right);
        return Ok((mine, Some((right[0].0.clone(), beside))));
    }
    if depth > DEEP {
        return Err(txn.damaged("a tree deeper than any is: it leads back into itself".into()));
    }
    let (i, child) = child_for(page, key).ok_or_else(|| torn(txn, page))?;
    let (now, parted) = put_under(txn, child, key, value, depth + 1)?;
    if now == child && parted.is_none() {
        // The child was the transaction's own already: nothing of this page changes.
        return Ok((at, None));
    }
    let mine = txn.own(at, BRANCH)?;
    let page = txn.page_mut(mine)?;
    let pointer = if i == 0 { Some(PAGE_HEAD + 4) } else { cell(page, i - 1, 10).map(|cell| cell + 2) };
    let Some(pointer) = pointer else {
        return Err(txn.damaged("a branch of a tree holds what is not within it".into()));
    };
    page[pointer..pointer + 8].copy_from_slice(&now.to_le_bytes());
    let Some((from, beside)) = parted else { return Ok((mine, None)) };
    if insert(page, i, &[&u16::try_from(from.len()).unwrap_or(0).to_le_bytes(), &beside.to_le_bytes(), &from]) {
        return Ok((mine, None));
    }
    let Some(mut branch) = Branch::read(page) else {
        return Err(txn.damaged("a branch of a tree holds what is not within it".into()));
    };
    branch.cells.insert(i.min(branch.cells.len()), (from, beside));
    if Branch::size(&branch.cells) <= PAGE {
        Branch::write(page, branch.first, &branch.cells);
        return Ok((mine, None));
    }
    // The middle key goes up: its child is the first of the page beside.
    let sizes: Vec<usize> = branch.cells.iter().map(|(key, _)| 12 + key.len()).collect();
    let mut right = branch.cells.split_off(part(&sizes, true));
    let (up, first) = right.remove(0);
    Branch::write(page, branch.first, &branch.cells);
    let beside = txn.make(BRANCH)?;
    Branch::write(txn.page_mut(beside)?, first, &right);
    Ok((mine, Some((up, beside))))
}

/// Give a key a value in a tree of the space, in place of the one it had.
///
/// # Errors
/// The key or the value is longer than a tree holds; no room can be made; or the space is damaged.
pub fn put(txn: &mut Txn<'_>, tree: usize, key: &[u8], value: &[u8]) -> Result<()> {
    if key.len() > KEY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "a key longer than a tree holds").into());
    }
    let root = txn.root(tree);
    if root == 0 {
        let new = held(txn, key.len(), value)?;
        let leaf = txn.make(LEAF)?;
        Leaf::write(txn.page_mut(leaf)?, &[(key.to_vec(), new)]);
        txn.set_root(tree, leaf);
        return Ok(());
    }
    let (at, parted) = put_under(txn, root, key, value, 0)?;
    let root = match parted {
        // Parted at the top: a branch over the two.
        Some((from, beside)) => {
            let above = txn.make(BRANCH)?;
            Branch::write(txn.page_mut(above)?, at, &[(from, beside)]);
            above
        }
        None => at,
    };
    txn.set_root(tree, root);
    Ok(())
}

/// Fill a tree that holds nothing with keys given in their order, each with its value: its pages filled as far as
/// leaves room for what is put after (`FILLED`), where keys put one by one leave them half full - what a
/// structure made whole is made by.
///
/// # Errors
/// The tree holds something; a key is not after the one before it, or longer than a tree holds; no room can be
/// made; or the space is damaged.
pub fn load(txn: &mut Txn<'_>, tree: usize, cells: &mut dyn Iterator<Item = (Vec<u8>, Vec<u8>)>) -> Result<()> {
    let invalid = |why: &str| Error::from(std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_string()));
    if txn.root(tree) != 0 {
        return Err(invalid("a tree is loaded whole where it holds nothing"));
    }
    // The leaves, each by its first key.
    let mut level: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut leaf: Vec<(Vec<u8>, Held)> = Vec::new();
    let mut last: Option<Vec<u8>> = None;
    for (key, value) in cells {
        if key.len() > KEY {
            return Err(invalid("a key longer than a tree holds"));
        }
        if last.as_ref().is_some_and(|last| *last >= key) {
            return Err(invalid("a tree is loaded with its keys in their order"));
        }
        let value = held(txn, key.len(), &value)?;
        let cell = (key, value);
        if !leaf.is_empty() && Leaf::size(&leaf) + Leaf::cell_size(&cell) > FILLED {
            let at = txn.make(LEAF)?;
            Leaf::write(txn.page_mut(at)?, &leaf);
            level.push((leaf[0].0.clone(), at));
            leaf.clear();
        }
        last = Some(cell.0.clone());
        leaf.push(cell);
    }
    if !leaf.is_empty() {
        let at = txn.make(LEAF)?;
        Leaf::write(txn.page_mut(at)?, &leaf);
        level.push((leaf[0].0.clone(), at));
    }
    // The branches over them, a level at a time, until one page is over all.
    while level.len() > 1 {
        let mut groups: Vec<Vec<(Vec<u8>, u64)>> = Vec::new();
        for child in level {
            let full = groups.last().is_some_and(|group| Branch::size(&group[1..]) + 12 + child.0.len() > FILLED);
            if full || groups.is_empty() {
                groups.push(Vec::new());
            }
            if let Some(group) = groups.last_mut() {
                group.push(child);
            }
        }
        // No branch over one child alone: the last takes one from the one before it.
        if let [.., before, last] = groups.as_mut_slice()
            && last.len() == 1
            && let Some(moved) = before.pop()
        {
            last.insert(0, moved);
        }
        let mut above = Vec::with_capacity(groups.len());
        for group in groups {
            let at = txn.make(BRANCH)?;
            Branch::write(txn.page_mut(at)?, group[0].1, &group[1..]);
            above.push((group[0].0.clone(), at));
        }
        level = above;
    }
    txn.set_root(tree, level.first().map_or(0, |(_, at)| *at));
    Ok(())
}

/// What removing a key under a page came to: whether it was there, and the page that holds the rest now - none
/// where nothing is left under it.
type Removed = (bool, Option<u64>);

fn remove_under(txn: &mut Txn<'_>, at: u64, key: &[u8], depth: usize) -> Result<Removed> {
    let page = txn.page_either(at, LEAF, BRANCH)?;
    if page[0] == LEAF {
        let Ok(i) = find(page, key).ok_or_else(|| torn(txn, page))? else { return Ok((false, Some(at))) };
        let cells = count(page);
        if let Value::Long { len, first } = leaf_cell(page, i).ok_or_else(|| torn(txn, page))?.1 {
            let_go_long(txn, len, first)?;
        }
        if cells == 1 {
            txn.let_go(at);
            return Ok((true, None));
        }
        let mine = txn.own(at, LEAF)?;
        take_out(txn.page_mut(mine)?, i);
        return Ok((true, Some(mine)));
    }
    if depth > DEEP {
        return Err(txn.damaged("a tree deeper than any is: it leads back into itself".into()));
    }
    let (i, child) = child_for(page, key).ok_or_else(|| torn(txn, page))?;
    let (cells, first) = (count(page), u64_at(page, PAGE_HEAD + 4));
    let second = if cells > 0 { Some(branch_cell(page, 0).ok_or_else(|| torn(txn, page))?.1) } else { None };
    let (was, left) = remove_under(txn, child, key, depth + 1)?;
    if !was {
        return Ok((false, Some(at)));
    }
    match left {
        // The child was the transaction's own already: nothing of this page changes.
        Some(now) if now == child => Ok((true, Some(at))),
        Some(now) => {
            let mine = txn.own(at, BRANCH)?;
            let page = txn.page_mut(mine)?;
            let pointer = if i == 0 { Some(PAGE_HEAD + 4) } else { cell(page, i - 1, 10).map(|cell| cell + 2) };
            let Some(pointer) = pointer else {
                return Err(txn.damaged("a branch of a tree holds what is not within it".into()));
            };
            page[pointer..pointer + 8].copy_from_slice(&now.to_le_bytes());
            Ok((true, Some(mine)))
        }
        // The child is gone, and it was the only one: nothing is left under this page.
        None if cells == 0 => {
            txn.let_go(at);
            Ok((true, None))
        }
        // One child is left and no key: the child stands for the branch.
        None if cells == 1 => {
            txn.let_go(at);
            Ok((true, if i == 0 { second } else { Some(first) }))
        }
        // The child's key goes with it - or, the first child gone, the one after it takes its place.
        None => {
            let mine = txn.own(at, BRANCH)?;
            let page = txn.page_mut(mine)?;
            if i == 0 {
                page[PAGE_HEAD + 4..PAGE_HEAD + 12].copy_from_slice(&second.unwrap_or(0).to_le_bytes());
                take_out(page, 0);
            } else {
                take_out(page, i - 1);
            }
            Ok((true, Some(mine)))
        }
    }
}

/// Take a key out of a tree of the space: whether it was there.
///
/// # Errors
/// The space is damaged.
pub fn remove(txn: &mut Txn<'_>, tree: usize, key: &[u8]) -> Result<bool> {
    let root = txn.root(tree);
    if root == 0 {
        return Ok(false);
    }
    let (was, left) = remove_under(txn, root, key, 0)?;
    txn.set_root(tree, left.unwrap_or(0));
    Ok(was)
}
