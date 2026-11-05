//! An option of a registered type or an array, as the inputs edit it: as an inspector lists one, an array's elements and a struct's
//! fields each a group of its own, down to its integers, each a row. What a package sets is the least that differs from the default - a struct's fields
//! by name, an array of structs by element - as the build merges it; an integer is held to its type's range before it is written.

use pe2_ext::{Table, Value};

/// A step into a value: a struct's field, or an array's element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Field(String),
    Index(usize),
}

fn count_of(shape: &Table) -> Option<usize> {
    shape.get("count").and_then(Value::as_integer).and_then(|count| usize::try_from(count).ok())
}

/// What an integer's type allows, when the shape is one.
#[must_use]
pub fn range_of(shape: &Table) -> Option<(i64, i64)> {
    Some((shape.get("low")?.as_integer()?, shape.get("high")?.as_integer()?))
}

fn text<'a>(table: &'a Table, key: &str) -> &'a str {
    table.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn fields(shape: &Table) -> Vec<&Table> {
    shape.get("fields").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table).collect()
}

/// Whether a shape is a dynamic array: an option's alone, as long as what it holds.
#[must_use]
pub fn is_dynamic(shape: &Table) -> bool {
    shape.get("dynamic").and_then(Value::as_bool) == Some(true)
}

/// A shape less its count: what one element of it is.
fn element(shape: &Table) -> Table {
    let mut one = shape.clone();
    one.remove("count");
    one.remove("dynamic");
    one
}

/// What is inside a shape, as an inspector lists it: an array's elements - a dynamic one's as many as `value`, the value in force,
/// holds - and a struct's fields, each with the step to it, how a row names it and its own shape; nothing for an integer.
#[must_use]
pub fn children(shape: &Table, value: Option<&Value>) -> Vec<(Step, String, Table)> {
    let count = if is_dynamic(shape) { value.and_then(Value::as_array).map(Vec::len) } else { count_of(shape) };
    if let Some(count) = count {
        let one = element(shape);
        return (0..count).map(|index| (Step::Index(index), format!("Element {index}"), one.clone())).collect();
    }
    fields(shape)
        .into_iter()
        .map(|field| (Step::Field(text(field, "name").to_string()), text(field, "name").to_string(), field.clone()))
        .collect()
}

/// What a row's name says when pointed at: a field's comment, and what an integer's type holds.
#[must_use]
pub fn about(shape: &Table) -> String {
    let range = range_of(shape).map(|(low, high)| format!("{}, {low} to {high}", text(shape, "type")));
    let doc = text(shape, "doc");
    match (doc.is_empty(), range) {
        (true, Some(range)) => range,
        (false, Some(range)) => format!("{doc}\n{range}"),
        (_, None) => {
            if doc.is_empty() {
                text(shape, "type").to_string()
            } else {
                format!("{doc}\n{}", text(shape, "type"))
            }
        }
    }
}

/// What a value holds at a path, if it holds it.
#[must_use]
pub fn at<'a>(value: Option<&'a Value>, path: &[Step]) -> Option<&'a Value> {
    path.iter().try_fold(value?, |value, step| match step {
        Step::Field(name) => value.get(name.as_str()),
        Step::Index(index) => value.as_array()?.get(*index),
    })
}

/// The value in force: the default with what the package sets merged in, as the build merges it - a dynamic array as long as what is
/// set - and zero for an integer neither gives.
#[must_use]
pub fn effective(shape: &Table, default: Option<&Value>, given: Option<&Value>) -> Value {
    let dynamic = is_dynamic(shape);
    if let Some(count) = count_of(shape).or_else(|| dynamic.then_some(0)) {
        let one = element(shape);
        let defaults = default.and_then(Value::as_array).cloned().unwrap_or_default();
        let set = given.and_then(Value::as_array);
        let length = if dynamic { set.map_or(defaults.len().max(1), Vec::len) } else { count };
        return Value::Array(
            (0..length)
                .map(|index| effective(&one, defaults.get(index), set.and_then(|list| list.get(index))))
                .collect(),
        );
    }
    if range_of(shape).is_some() {
        return given.filter(|value| value.is_integer()).or(default).cloned().unwrap_or(Value::Integer(0));
    }
    let table = fields(shape).into_iter().map(|field| {
        let name = text(field, "name");
        (
            name.to_string(),
            effective(field, default.and_then(|value| value.get(name)), given.and_then(|value| value.get(name))),
        )
    });
    Value::Table(table.collect())
}

/// What a package sets for a value in force: the least that differs from the default, none where nothing does, as the build merges
/// it. An array of integers is written whole; an array of structs element by element, one that differs in nothing an empty table, one
/// past the default's in full; a dynamic array whose length is not the default's is written whatever it holds.
#[must_use]
pub fn minimal(shape: &Table, default: Option<&Value>, full: &Value) -> Option<Value> {
    if count_of(shape).is_some() || is_dynamic(shape) {
        let one = element(shape);
        let defaults = default.and_then(Value::as_array).cloned().unwrap_or_default();
        let items = full.as_array().cloned().unwrap_or_default();
        if range_of(&one).is_some() {
            return (default != Some(full)).then(|| full.clone());
        }
        let list: Vec<Value> = items
            .iter()
            .enumerate()
            .map(|(index, item)| match defaults.get(index) {
                Some(known) => minimal(&one, Some(known), item).unwrap_or_else(|| Value::Table(Table::new())),
                None => item.clone(),
            })
            .collect();
        let unchanged =
            items.len() == defaults.len() && list.iter().all(|value| value.as_table().is_some_and(Table::is_empty));
        return (!unchanged).then_some(Value::Array(list));
    }
    if range_of(shape).is_some() {
        return (default != Some(full)).then(|| full.clone());
    }
    let table: Table = fields(shape)
        .into_iter()
        .filter_map(|field| {
            let name = text(field, "name");
            let inner = minimal(field, default.and_then(|value| value.get(name)), full.get(name)?)?;
            Some((name.to_string(), inner))
        })
        .collect();
    (!table.is_empty()).then_some(Value::Table(table))
}

/// The value at a path, made writable.
fn at_mut<'a>(value: &'a mut Value, path: &[Step]) -> Option<&'a mut Value> {
    path.iter().try_fold(value, |value, step| match step {
        Step::Field(name) => value.as_table_mut()?.get_mut(name.as_str()),
        Step::Index(index) => value.as_array_mut()?.get_mut(*index),
    })
}

/// What a package's value becomes with an element added to a dynamic array at `path` (the whole option, at an empty one): a copy of
/// its last.
#[must_use]
pub fn add(shape: &Table, given: Option<&Value>, default: Option<&Value>) -> Option<Value> {
    let mut full = effective(shape, default, given);
    if let Some(list) = full.as_array_mut()
        && let Some(last) = list.last().cloned()
    {
        list.push(last);
    }
    minimal(shape, default, &full)
}

/// What a package's value becomes with a dynamic array's element taken out, its last never.
#[must_use]
pub fn remove(shape: &Table, given: Option<&Value>, default: Option<&Value>, index: usize) -> Option<Value> {
    let mut full = effective(shape, default, given);
    if let Some(list) = full.as_array_mut().filter(|list| list.len() > 1 && index < list.len()) {
        list.remove(index);
    }
    minimal(shape, default, &full)
}

/// What a package's value becomes with one integer set (or, with none, taken back to the default): the least that differs from the
/// default (`minimal`), none where nothing does.
#[must_use]
pub fn set(
    shape: &Table,
    given: Option<&Value>,
    default: Option<&Value>,
    path: &[Step],
    new: Option<i64>,
) -> Option<Value> {
    let mut full = effective(shape, default, given);
    let back = at(default, path).cloned().unwrap_or(Value::Integer(0));
    if let Some(slot) = at_mut(&mut full, path) {
        *slot = new.map_or(back, Value::Integer);
    }
    minimal(shape, default, &full)
}

/// An integer as typed, in decimal or hexadecimal, signed.
#[must_use]
pub fn typed_number(typed: &str) -> Option<i64> {
    let typed = typed.trim();
    let (negative, digits) = typed.strip_prefix('-').map_or((false, typed), |rest| (true, rest.trim_start()));
    let magnitude = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) => i64::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// Why what was typed in a cell cannot be written; none where it can, as the number it is.
pub fn checked(typed: &str, leaf_ty: &str, low: i64, high: i64) -> Result<i64, String> {
    let number = typed_number(typed).ok_or_else(|| format!("{} is no integer", typed.trim()))?;
    if (low..=high).contains(&number) { Ok(number) } else { Err(format!("a {leaf_ty} is {low} to {high}")) }
}

/// Why what a package sets cannot be merged into its option, as the build would say; none where it can.
#[must_use]
pub fn wrong(shape: &Table, given: &Value) -> Option<String> {
    if is_dynamic(shape) {
        let Some(list) = given.as_array().filter(|list| !list.is_empty()) else {
            return Some(format!("is one or more of {}", text(shape, "type")));
        };
        let one = element(shape);
        return list.iter().find_map(|value| wrong(&one, value));
    }
    if let Some(count) = count_of(shape) {
        let Some(list) = given.as_array().filter(|list| list.len() == count) else {
            return Some(format!("is {count} of {}", text(shape, "type")));
        };
        let one = element(shape);
        return list.iter().find_map(|value| wrong(&one, value));
    }
    if let Some((low, high)) = range_of(shape) {
        let fits = given.as_integer().is_some_and(|number| (low..=high).contains(&number));
        return (!fits).then(|| format!("a {} is {low} to {high}", text(shape, "type")));
    }
    let Some(table) = given.as_table() else {
        return Some(format!("a {} is set by a table of its fields", text(shape, "type")));
    };
    let known = fields(shape);
    if let Some(name) = table.keys().find(|name| !known.iter().any(|field| text(field, "name") == name.as_str())) {
        return Some(format!("a {} has no field {name}", text(shape, "type")));
    }
    known.iter().find_map(|field| table.get(text(field, "name")).and_then(|value| wrong(field, value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Table {
        let text = "type = '_Level'\ncount = 2\n\
            [[fields]]\nname = 'wedges'\ntype = 's16'\nlow = -32768\nhigh = 32767\ndoc = 'Fanned'\n\
            [[fields]]\nname = 'steps'\ntype = 'u8'\ncount = 2\nlow = 0\nhigh = 255\n";
        text.parse().unwrap()
    }

    fn default() -> Value {
        "v = [{ wedges = 8, steps = [1, 2] }, { wedges = 12, steps = [3, 4] }]".parse::<Table>().unwrap()["v"].clone()
    }

    #[test]
    fn an_inspector_lists_elements_then_fields() {
        let elements = children(&shape(), None);
        let names: Vec<&str> = elements.iter().map(|(_, name, _)| name.as_str()).collect();
        assert_eq!(names, ["Element 0", "Element 1"]);
        let fields = children(&elements[1].2, None);
        assert_eq!(fields.iter().map(|(_, name, _)| name.as_str()).collect::<Vec<_>>(), ["wedges", "steps"]);
        assert_eq!(about(&fields[0].2), "Fanned\ns16, -32768 to 32767");
        let steps = children(&fields[1].2, None);
        assert_eq!((steps.len(), range_of(&steps[1].2)), (2, Some((0, 255))));
        let path = [Step::Index(1), Step::Field("steps".into()), Step::Index(1)];
        assert_eq!(at(Some(&default()), &path).and_then(Value::as_integer), Some(4));
    }

    #[test]
    fn a_change_writes_the_least_that_differs_and_taking_it_back_writes_nothing() {
        let (shape, default) = (shape(), default());
        let wedges = |index: usize| vec![Step::Index(index), Step::Field("wedges".into())];
        let step = |index: usize, at: usize| vec![Step::Index(index), Step::Field("steps".into()), Step::Index(at)];
        let set_wedges = set(&shape, None, Some(&default), &wedges(1), Some(9)).unwrap();
        assert_eq!(set_wedges.to_string(), "[{}, { wedges = 9 }]");
        let set_step = set(&shape, Some(&set_wedges), Some(&default), &step(0, 1), Some(7)).unwrap();
        assert_eq!(set_step.to_string(), "[{ steps = [1, 7] }, { wedges = 9 }]");
        let back = set(&shape, Some(&set_step), Some(&default), &step(0, 1), None).unwrap();
        assert_eq!(back.to_string(), "[{}, { wedges = 9 }]");
        assert_eq!(set(&shape, Some(&back), Some(&default), &wedges(1), None), None);
    }

    #[test]
    fn a_dynamic_array_grows_and_shrinks_and_writes_what_differs() {
        let mut shape = shape();
        shape.remove("count");
        shape.insert("dynamic".into(), true.into());
        let default = default();
        let grown = add(&shape, None, Some(&default)).unwrap();
        assert_eq!(grown.to_string(), "[{}, {}, { wedges = 12, steps = [3, 4] }]");
        assert_eq!(children(&shape, Some(&effective(&shape, Some(&default), Some(&grown)))).len(), 3);
        let first_out = remove(&shape, Some(&grown), Some(&default), 0).unwrap();
        assert_eq!(first_out.to_string(), "[{ wedges = 12, steps = [3, 4] }, {}]");
        let back = remove(&shape, Some(&grown), Some(&default), 2);
        assert_eq!(back, None, "back to the default's two, nothing is written");
        assert_eq!(wrong(&shape, &grown), None);
        assert_eq!(wrong(&shape, &Value::Array(vec![])).as_deref(), Some("is one or more of _Level"));
        let last = remove(&shape, None, Some(&default), 1).unwrap();
        assert_eq!(remove(&shape, Some(&last), Some(&default), 0), Some(last), "the last one stays");
    }

    #[test]
    fn what_cannot_be_held_is_refused() {
        assert_eq!(checked("0x10", "u8", 0, 255), Ok(16));
        assert_eq!(checked("300", "u8", 0, 255), Err("a u8 is 0 to 255".to_string()));
        assert_eq!(checked("x", "u8", 0, 255), Err("x is no integer".to_string()));
        let given: Value = "v = [{ wedges = 40000 }, {}]".parse::<Table>().unwrap()["v"].clone();
        assert_eq!(wrong(&shape(), &given).as_deref(), Some("a s16 is -32768 to 32767"));
        let short: Value = "v = [{}]".parse::<Table>().unwrap()["v"].clone();
        assert_eq!(wrong(&shape(), &short).as_deref(), Some("is 2 of _Level"));
        assert_eq!(wrong(&shape(), &default()), None);
    }
}
