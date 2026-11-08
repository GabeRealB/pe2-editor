//! Three-way merges of TOML by field: what `mine` changed of `base`, field by field, set over `theirs`, the version
//! that changed below - its layout and comments kept. A field's path goes through tables by key, and through an array
//! of tables by each element's `id`, or by its place where it has none, as long as the array keeps its length and its
//! ids in their places (a node's `stack`); any other array is one field, replaced whole. A field both sides changed,
//! to different values, conflicts: mine is kept.

use super::Conflict;

/// A field one version changes of another: its path, what it is now (none: removed) and what it was (none: absent).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub path: Vec<Step>,
    pub value: Option<toml::Value>,
    pub was: Option<toml::Value>,
}

/// A step of a field's path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// A table's key.
    Key(String),
    /// The element of an array of tables whose `id` this is.
    Id(String),
    /// The element of an array of tables at this place, one without an `id`.
    At(usize),
}

/// The step each element of an array of tables is reached by: its `id`, else its place; none for any other array, or
/// where two elements share an id.
fn steps(array: &[toml::Value]) -> Option<Vec<Step>> {
    let steps: Option<Vec<Step>> = array
        .iter()
        .enumerate()
        .map(|(at, e)| {
            let table = e.as_table()?;
            Some(match table.get("id") {
                Some(id) => Step::Id(id.as_str()?.to_string()),
                None => Step::At(at),
            })
        })
        .collect();
    let steps = steps?;
    let ids: std::collections::BTreeSet<&Step> = steps.iter().collect();
    (ids.len() == steps.len() && !steps.is_empty()).then_some(steps)
}

/// The fields one declaration changes of another, `base` to `new`.
#[must_use]
pub fn diff(base: &toml::Table, new: &toml::Table) -> Vec<Change> {
    let mut fields = Vec::new();
    diff_into(&mut Vec::new(), base, new, &mut fields);
    fields
}

fn diff_into(at: &mut Vec<Step>, base: &toml::Table, new: &toml::Table, out: &mut Vec<Change>) {
    let keys = base.keys().chain(new.keys().filter(|k| !base.contains_key(*k)));
    for key in keys.cloned().collect::<Vec<_>>() {
        at.push(Step::Key(key.clone()));
        match (base.get(&key), new.get(&key)) {
            (Some(toml::Value::Table(b)), Some(toml::Value::Table(n))) => diff_into(at, b, n, out),
            (Some(toml::Value::Array(b)), Some(toml::Value::Array(n)))
                if steps(b).is_some() && steps(b) == steps(n) =>
            {
                for ((step, b), n) in steps(b).unwrap_or_default().into_iter().zip(b).zip(n) {
                    let (Some(bt), Some(nt)) = (b.as_table(), n.as_table()) else { continue };
                    at.push(step);
                    diff_into(at, bt, nt, out);
                    at.pop();
                }
            }
            (b, n) if b != n => out.push(Change { path: at.clone(), value: n.cloned(), was: b.cloned() }),
            _ => {}
        }
        at.pop();
    }
}

/// What a value is at a path.
#[must_use]
pub fn get<'a>(table: &'a toml::Table, path: &[Step]) -> Option<&'a toml::Value> {
    let (Step::Key(first), rest) = path.split_first()? else { return None };
    let mut value = table.get(first)?;
    for step in rest {
        value = match step {
            Step::Key(key) => value.as_table()?.get(key)?,
            Step::Id(id) => value.as_array()?.iter().find(|e| e.get("id").and_then(toml::Value::as_str) == Some(id))?,
            Step::At(at) => value.as_array()?.get(*at).filter(|e| e.get("id").is_none())?,
        };
    }
    Some(value)
}

/// A value as `toml_edit` writes it: tables as tables, arrays of tables as such, so a merged file reads as one written.
fn item(value: &toml::Value) -> toml_edit::Item {
    let holder = toml::Table::from_iter([("v".to_string(), value.clone())]);
    let text = toml::to_string(&holder).unwrap_or_default();
    text.parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|mut doc| doc.as_table_mut().remove("v"))
        .unwrap_or_else(|| toml_edit::value(value.to_string()))
}

/// The table of an array's element a step reaches: by its `id`, or by its place where it has none.
fn element<'a>(
    mut tables: impl Iterator<Item = &'a mut toml_edit::Table>,
    step: &Step,
) -> Option<&'a mut toml_edit::Table> {
    match step {
        Step::Id(id) => tables.find(|t| t.get("id").and_then(toml_edit::Item::as_str) == Some(id)),
        Step::At(at) => tables.nth(*at).filter(|t| !t.contains_key("id")),
        Step::Key(_) => None,
    }
}

/// Set (or with none remove) a value at a path inside a table, making the tables it passes through where missing; a
/// value it replaces keeps its comment and spacing.
pub(crate) fn set_in_table(table: &mut toml_edit::Table, path: &[Step], value: Option<&toml::Value>) {
    let Some((Step::Key(first), rest)) = path.split_first() else { return };
    if rest.is_empty() {
        match value {
            Some(value) => match (table.get_mut(first), item(value)) {
                // Replaced in place: the key keeps what is written before it (a comment), the value its decor.
                (Some(toml_edit::Item::Value(old)), toml_edit::Item::Value(mut new)) => {
                    *new.decor_mut() = old.decor().clone();
                    *old = new;
                }
                (_, new) => {
                    table.insert(first, new);
                }
            },
            None => {
                table.remove(first);
            }
        }
        return;
    }
    if (value.is_none() || !matches!(rest[0], Step::Key(_))) && table.get(first).is_none() {
        return;
    }
    let next = table.entry(first).or_insert_with(|| {
        let mut new = toml_edit::Table::new();
        new.set_implicit(true);
        toml_edit::Item::Table(new)
    });
    match (next, &rest[0]) {
        (toml_edit::Item::ArrayOfTables(array), step @ (Step::Id(_) | Step::At(_))) => {
            if let Some(inner) = element(array.iter_mut(), step) {
                set_in_table(inner, &rest[1..], value);
            }
        }
        (toml_edit::Item::Value(toml_edit::Value::Array(array)), step @ (Step::Id(_) | Step::At(_))) => {
            set_in_array(array, step, &rest[1..], value);
        }
        (_, Step::Id(_) | Step::At(_)) => {}
        (toml_edit::Item::Table(inner), _) => set_in_table(inner, rest, value),
        (toml_edit::Item::Value(toml_edit::Value::InlineTable(inner)), _) => set_in_inline(inner, rest, value),
        (other, _) => {
            let mut inner = toml_edit::Table::new();
            set_in_table(&mut inner, rest, value);
            *other = toml_edit::Item::Table(inner);
        }
    }
}

/// Set a value inside the inline table of an array a step reaches: by its `id`, or by its place where it has none.
fn set_in_array(array: &mut toml_edit::Array, step: &Step, path: &[Step], value: Option<&toml::Value>) {
    let found = match step {
        Step::Id(id) => array
            .iter_mut()
            .filter_map(toml_edit::Value::as_inline_table_mut)
            .find(|t| t.get("id").and_then(toml_edit::Value::as_str) == Some(id)),
        Step::At(at) => {
            array.get_mut(*at).and_then(toml_edit::Value::as_inline_table_mut).filter(|t| !t.contains_key("id"))
        }
        Step::Key(_) => None,
    };
    if let Some(inner) = found {
        set_in_inline(inner, path, value);
    }
}

/// Set (or with none remove) a value at a path inside an inline table, as written: inline.
fn set_in_inline(table: &mut toml_edit::InlineTable, path: &[Step], value: Option<&toml::Value>) {
    let Some((Step::Key(first), rest)) = path.split_first() else { return };
    if rest.is_empty() {
        match value.map(item).map(|i| i.into_value().ok()) {
            Some(Some(mut new)) => match table.get_mut(first) {
                Some(old) => {
                    *new.decor_mut() = old.decor().clone();
                    *old = new;
                }
                None => {
                    table.insert(first, new);
                }
            },
            Some(None) => {}
            None => {
                table.remove(first);
            }
        }
        return;
    }
    if (value.is_none() || !matches!(rest[0], Step::Key(_))) && table.get(first).is_none() {
        return;
    }
    let next = table.entry(first).or_insert(toml_edit::Value::InlineTable(toml_edit::InlineTable::new()));
    match (next, &rest[0]) {
        (toml_edit::Value::Array(array), step @ (Step::Id(_) | Step::At(_))) => {
            set_in_array(array, step, &rest[1..], value);
        }
        (_, Step::Id(_) | Step::At(_)) => {}
        (toml_edit::Value::InlineTable(inner), _) => set_in_inline(inner, rest, value),
        (other, _) => {
            let mut inner = toml_edit::InlineTable::new();
            set_in_inline(&mut inner, rest, value);
            *other = toml_edit::Value::InlineTable(inner);
        }
    }
}

/// A path as a person reads it: `stack.[ufo].style`.
#[must_use]
pub fn shown(path: &[Step]) -> String {
    let steps: Vec<String> = path
        .iter()
        .map(|step| match step {
            Step::Key(key) => key.clone(),
            Step::Id(id) => format!("[{id}]"),
            Step::At(at) => format!("[{at}]"),
        })
        .collect();
    steps.join(".")
}

/// Merge `mine` over `theirs`, both changed from `base`, field by field: the text, and the fields the two changed
/// otherwise; none where one of them is no TOML table.
#[must_use]
pub fn merge(base: &str, theirs: &str, mine: &str) -> Option<(String, Vec<Conflict>)> {
    let base: toml::Table = toml::from_str(base).ok()?;
    let mine: toml::Table = toml::from_str(mine).ok()?;
    let below: toml::Table = toml::from_str(theirs).ok()?;
    let mut doc: toml_edit::DocumentMut = theirs.parse().ok()?;
    let mut conflicts = Vec::new();
    for change in diff(&base, &mine) {
        let now = get(&below, &change.path);
        if now == change.value.as_ref() {
            continue;
        }
        if now != change.was.as_ref() {
            conflicts.push(Conflict {
                at: Some(shown(&change.path)),
                mine: change.value.as_ref().map(ToString::to_string),
                base: change.was.as_ref().map(ToString::to_string),
                theirs: now.map(ToString::to_string),
            });
        }
        set_in_table(doc.as_table_mut(), &change.path, change.value.as_ref());
    }
    Some((doc.to_string(), conflicts))
}

#[cfg(test)]
mod tests {
    use super::merge;

    #[test]
    fn a_change_below_reaches_through_with_the_layout_kept() {
        let base = "# a node\nsize = 2 # its size\nname = \"a\"\n";
        let theirs = "# a node\nsize = 2 # its size\nname = \"b\"\n";
        let mine = "size = 3\nname = \"a\"\n";
        let (merged, conflicts) = merge(base, theirs, mine).unwrap();
        assert_eq!(merged, "# a node\nsize = 3 # its size\nname = \"b\"\n");
        assert_eq!(conflicts, vec![]);
    }

    #[test]
    fn a_field_changed_both_ways_conflicts_and_mine_is_kept() {
        let (merged, conflicts) = merge("size = 2\n", "size = 5\n", "size = 3\n").unwrap();
        assert_eq!(merged, "size = 3\n");
        let c = &conflicts[0];
        assert_eq!(
            (c.at.as_deref(), c.mine.as_deref(), c.base.as_deref(), c.theirs.as_deref()),
            (Some("size"), Some("3"), Some("2"), Some("5"))
        );
    }

    #[test]
    fn a_stack_merges_by_its_ids_whatever_its_order_below() {
        let base = "[[stack]]\nid = \"a\"\nop = \"x\"\n\n[[stack]]\nid = \"b\"\nop = \"y\"\n";
        let mine = "[[stack]]\nid = \"a\"\nop = \"x\"\n\n[[stack]]\nid = \"b\"\nop = \"z\"\n";
        let theirs = "[[stack]]\nid = \"b\"\nop = \"y\"\nstyle = 1\n\n[[stack]]\nid = \"a\"\nop = \"x\"\n";
        let (merged, conflicts) = merge(base, theirs, mine).unwrap();
        assert_eq!(conflicts, vec![]);
        assert_eq!(merged, "[[stack]]\nid = \"b\"\nop = \"z\"\nstyle = 1\n\n[[stack]]\nid = \"a\"\nop = \"x\"\n");
    }

    #[test]
    fn a_field_removed_and_one_added_merge_and_text_that_is_no_toml_does_not() {
        let (merged, _) = merge("a = 1\nb = 2\n", "a = 1\nb = 2\nc = 3\n", "b = 2\nd = 4\n").unwrap();
        assert_eq!(merged, "b = 2\nc = 3\nd = 4\n");
        assert!(merge("a = 1\n", "a = [", "a = 2\n").is_none());
    }
}
