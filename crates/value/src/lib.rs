//! Values as the editor and its tools hold them: a node's fields, what its import made, its settings resolved. TOML is
//! what people write them as; in memory they are these, which hold a project's thousands of them at a fraction of what
//! `toml::Value` takes:
//!
//! - strings and keys are kept once (`Str`, interned where short: the same few hundred names recur in every node);
//! - an array of numbers alike is packed (`Integers`, `Floats`: a palette, a rect), 8 bytes an element;
//! - tables and arrays are immutable and shared (`Arc`): a copy costs a reference, a change copies only what it changes
//!   (`Table::insert` and the like, copy on write), so what is resolved over a node's fields shares what it leaves be;
//! - a table keeps its entries in the order written, found through a sorted index past a few.
//!
//! They read and write as TOML (`serde`: `toml::from_str::<Table>`, `toml::to_string`), and as CBOR where nobody reads
//! them; a datetime travels as TOML's own. Conversions both ways with `toml::Value` serve what has not moved yet.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

/// Strings at most this long are kept once, whoever makes them.
const INTERNED: usize = 48;

/// A string, kept once where short.
#[derive(Clone)]
pub struct Str(Arc<str>);

fn interned() -> &'static RwLock<HashSet<Arc<str>>> {
    static STRINGS: OnceLock<RwLock<HashSet<Arc<str>>>> = OnceLock::new();
    STRINGS.get_or_init(RwLock::default)
}

impl Str {
    #[must_use]
    pub fn new(text: &str) -> Str {
        if text.len() > INTERNED {
            return Str(Arc::from(text));
        }
        if let Some(found) = interned().read().unwrap_or_else(std::sync::PoisonError::into_inner).get(text) {
            return Str(found.clone());
        }
        let mut strings = interned().write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = strings.get(text) {
            return Str(found.clone());
        }
        let made: Arc<str> = Arc::from(text);
        strings.insert(made.clone());
        Str(made)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for Str {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Str) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Eq for Str {}

impl PartialOrd for Str {
    fn partial_cmp(&self, other: &Str) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Str {
    fn cmp(&self, other: &Str) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl std::hash::Hash for Str {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Str {
    fn from(text: &str) -> Str {
        Str::new(text)
    }
}

impl From<String> for Str {
    fn from(text: String) -> Str {
        Str::new(&text)
    }
}

/// A value.
#[derive(Clone, Debug)]
pub enum Value {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(Str),
    /// A TOML datetime, as written.
    Datetime(Str),
    /// An array of integers, packed.
    Integers(Arc<[i64]>),
    /// An array of floats, packed.
    Floats(Arc<[f64]>),
    /// Any other array.
    Array(Arc<[Value]>),
    Table(Table),
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) | (Value::Datetime(a), Value::Datetime(b)) => a == b,
            (Value::Integers(a), Value::Integers(b)) => Arc::ptr_eq(a, b) || a == b,
            (Value::Floats(a), Value::Floats(b)) => Arc::ptr_eq(a, b) || a == b,
            (Value::Array(a), Value::Array(b)) => Arc::ptr_eq(a, b) || a == b,
            (Value::Table(a), Value::Table(b)) => a == b,
            _ => false,
        }
    }
}

/// An array, however it is held: its items read one by one, numbers as values of their own.
#[derive(Clone, Copy, Debug)]
pub enum Items<'a> {
    Integers(&'a [i64]),
    Floats(&'a [f64]),
    Values(&'a [Value]),
}

impl<'a> Items<'a> {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Items::Integers(items) => items.len(),
            Items::Floats(items) => items.len(),
            Items::Values(items) => items.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The item at a place.
    #[must_use]
    pub fn get(&self, at: usize) -> Option<Value> {
        match self {
            Items::Integers(items) => items.get(at).map(|v| Value::Integer(*v)),
            Items::Floats(items) => items.get(at).map(|v| Value::Float(*v)),
            Items::Values(items) => items.get(at).cloned(),
        }
    }

    /// Its items, in order.
    pub fn iter(self) -> impl Iterator<Item = Value> + 'a {
        (0..self.len()).filter_map(move |at| self.get(at))
    }
}

impl Value {
    /// An array of these values: packed where they are all integers, or all floats.
    #[must_use]
    pub fn array(items: Vec<Value>) -> Value {
        if !items.is_empty() && items.iter().all(|v| matches!(v, Value::Integer(_))) {
            return Value::Integers(items.iter().filter_map(Value::as_integer).collect());
        }
        if !items.is_empty() && items.iter().all(|v| matches!(v, Value::Float(_))) {
            return Value::Floats(items.iter().filter_map(Value::as_float).collect());
        }
        Value::Array(items.into())
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Its items, where it is an array.
    #[must_use]
    pub fn items(&self) -> Option<Items<'_>> {
        match self {
            Value::Integers(items) => Some(Items::Integers(items)),
            Value::Floats(items) => Some(Items::Floats(items)),
            Value::Array(items) => Some(Items::Values(items)),
            _ => None,
        }
    }

    /// Its items as values, where it is an array of anything but packed numbers.
    #[must_use]
    pub fn as_values(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_table(&self) -> bool {
        matches!(self, Value::Table(_))
    }

    /// What kind of value it is, as TOML names it.
    #[must_use]
    pub fn type_str(&self) -> &'static str {
        match self {
            Value::Bool(_) => "boolean",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Datetime(_) => "datetime",
            Value::Integers(_) | Value::Floats(_) | Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }

    /// It as `toml` holds it.
    #[must_use]
    pub fn to_toml(&self) -> toml::Value {
        match self {
            Value::Bool(v) => toml::Value::Boolean(*v),
            Value::Integer(v) => toml::Value::Integer(*v),
            Value::Float(v) => toml::Value::Float(*v),
            Value::String(s) => toml::Value::String(s.to_string()),
            Value::Datetime(s) => s.parse().map_or_else(|_| toml::Value::String(s.to_string()), toml::Value::Datetime),
            Value::Integers(items) => toml::Value::Array(items.iter().map(|v| toml::Value::Integer(*v)).collect()),
            Value::Floats(items) => toml::Value::Array(items.iter().map(|v| toml::Value::Float(*v)).collect()),
            Value::Array(items) => toml::Value::Array(items.iter().map(Value::to_toml).collect()),
            Value::Table(t) => toml::Value::Table(t.to_toml()),
        }
    }
}

impl From<&toml::Value> for Value {
    fn from(value: &toml::Value) -> Value {
        match value {
            toml::Value::Boolean(v) => Value::Bool(*v),
            toml::Value::Integer(v) => Value::Integer(*v),
            toml::Value::Float(v) => Value::Float(*v),
            toml::Value::String(s) => Value::String(Str::new(s)),
            toml::Value::Datetime(d) => Value::Datetime(Str::new(&d.to_string())),
            toml::Value::Array(items) => Value::array(items.iter().map(Value::from).collect()),
            toml::Value::Table(t) => Value::Table(Table::from(t)),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Value {
        Value::Integer(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Float(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::String(Str::new(v))
    }
}

impl From<Table> for Value {
    fn from(t: Table) -> Value {
        Value::Table(t)
    }
}

/// Tables this long or longer are searched through a sorted index; shorter ones, in order.
const INDEXED: usize = 9;

/// A table: its entries in the order written, shared.
#[derive(Clone, Default)]
pub struct Table(Arc<Entries>);

#[derive(Clone, Default)]
struct Entries {
    list: Vec<(Str, Value)>,
    /// The entries' places, by their keys: made when first searched, made again once changed.
    sorted: OnceLock<Box<[u32]>>,
}

impl Entries {
    fn find(&self, key: &str) -> Option<usize> {
        if self.list.len() < INDEXED {
            return self.list.iter().position(|(k, _)| &**k == key);
        }
        let sorted = self.sorted.get_or_init(|| {
            let mut places: Vec<u32> = (0..u32::try_from(self.list.len()).unwrap_or(u32::MAX)).collect();
            places.sort_by(|a, b| self.list[*a as usize].0.cmp(&self.list[*b as usize].0));
            places.into_boxed_slice()
        });
        let at = sorted.binary_search_by(|place| (*self.list[*place as usize].0).cmp(key)).ok()?;
        Some(sorted[at] as usize)
    }
}

impl Table {
    #[must_use]
    pub fn new() -> Table {
        Table::default()
    }

    /// A table of these entries, in this order; where a key is twice, its last value.
    #[must_use]
    pub fn from_entries(entries: Vec<(Str, Value)>) -> Table {
        let mut table = Table::new();
        let inner = Arc::make_mut(&mut table.0);
        let mut seen: HashSet<Str> = HashSet::with_capacity(entries.len());
        for (key, value) in entries {
            if seen.insert(key.clone()) {
                inner.list.push((key, value));
            } else if let Some(at) = inner.list.iter().position(|(k, _)| *k == key) {
                inner.list[at].1 = value;
            }
        }
        inner.list.shrink_to_fit();
        table
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.find(key).map(|at| &self.0.list[at].1)
    }

    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.find(key).is_some()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.list.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.list.is_empty()
    }

    /// Its entries, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.list.iter().map(|(k, v)| (&**k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.list.iter().map(|(k, _)| &**k)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.0.list.iter().map(|(_, v)| v)
    }

    /// Whether two tables are the same one, shared.
    #[must_use]
    pub fn ptr_eq(a: &Table, b: &Table) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }

    /// Set a key's value, after the others where it is new: what was there before. The table is copied first where it
    /// is shared.
    pub fn insert(&mut self, key: impl Into<Str>, value: Value) -> Option<Value> {
        let key = key.into();
        let found = self.0.find(&key);
        let inner = Arc::make_mut(&mut self.0);
        if let Some(at) = found {
            return Some(std::mem::replace(&mut inner.list[at].1, value));
        }
        // A table is built whole and seldom grown: by one, not doubled.
        inner.list.reserve_exact(1);
        inner.list.push((key, value));
        inner.sorted = OnceLock::new();
        None
    }

    /// Remove a key: its value. Copied first where shared.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let at = self.0.find(key)?;
        let inner = Arc::make_mut(&mut self.0);
        inner.sorted = OnceLock::new();
        Some(inner.list.remove(at).1)
    }

    /// A key's value to change. Copied first where shared.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let at = self.0.find(key)?;
        Some(&mut Arc::make_mut(&mut self.0).list[at].1)
    }

    /// It as `toml` holds it.
    #[must_use]
    pub fn to_toml(&self) -> toml::Table {
        self.iter().map(|(k, v)| (k.to_string(), v.to_toml())).collect()
    }
}

impl From<&toml::Table> for Table {
    fn from(table: &toml::Table) -> Table {
        Table::from_entries(table.iter().map(|(k, v)| (Str::new(k), Value::from(v))).collect())
    }
}

impl PartialEq for Table {
    fn eq(&self, other: &Table) -> bool {
        Table::ptr_eq(self, other) || self.0.list == other.0.list
    }
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl FromIterator<(Str, Value)> for Table {
    fn from_iter<I: IntoIterator<Item = (Str, Value)>>(entries: I) -> Table {
        Table::from_entries(entries.into_iter().collect())
    }
}

mod serial {
    //! As TOML and CBOR: a table a map, an array a sequence, a datetime as TOML's own.

    use super::{Str, Table, Value};
    use serde::de::{self, MapAccess, SeqAccess, Visitor};
    use serde::ser::{SerializeMap, SerializeSeq};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::fmt;

    /// What `toml` names a datetime by, read and written as a table of this one key.
    const DATETIME: &str = "$__toml_private_datetime";

    impl Serialize for Value {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            match self {
                Value::Bool(v) => s.serialize_bool(*v),
                Value::Integer(v) => s.serialize_i64(*v),
                Value::Float(v) => s.serialize_f64(*v),
                Value::String(v) => s.serialize_str(v),
                Value::Datetime(v) => match v.parse::<toml::value::Datetime>() {
                    Ok(datetime) => datetime.serialize(s),
                    Err(_) => s.serialize_str(v),
                },
                Value::Integers(items) => {
                    let mut seq = s.serialize_seq(Some(items.len()))?;
                    for item in items.iter() {
                        seq.serialize_element(item)?;
                    }
                    seq.end()
                }
                Value::Floats(items) => {
                    let mut seq = s.serialize_seq(Some(items.len()))?;
                    for item in items.iter() {
                        seq.serialize_element(item)?;
                    }
                    seq.end()
                }
                Value::Array(items) => {
                    let mut seq = s.serialize_seq(Some(items.len()))?;
                    for item in items.iter() {
                        seq.serialize_element(item)?;
                    }
                    seq.end()
                }
                Value::Table(t) => t.serialize(s),
            }
        }
    }

    impl Serialize for Table {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(self.len()))?;
            for (k, v) in self.iter() {
                map.serialize_entry(k, v)?;
            }
            map.end()
        }
    }

    struct ValueVisitor;

    impl<'de> Visitor<'de> for ValueVisitor {
        type Value = Value;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a value")
        }

        fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
            Ok(Value::Bool(v))
        }

        fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
            Ok(Value::Integer(v))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
            i64::try_from(v).map(Value::Integer).map_err(|_| E::custom("an integer past i64"))
        }

        fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
            Ok(Value::Float(v))
        }

        fn visit_str<E>(self, v: &str) -> Result<Value, E> {
            Ok(Value::String(Str::new(v)))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 16));
            while let Some(item) = seq.next_element::<Value>()? {
                items.push(item);
            }
            Ok(Value::array(items))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
            let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0).min(1 << 16));
            while let Some(key) = map.next_key::<String>()? {
                if key == DATETIME {
                    // Its text, as written.
                    let text: String = map.next_value()?;
                    return Ok(Value::Datetime(Str::new(&text)));
                }
                entries.push((Str::new(&key), map.next_value::<Value>()?));
            }
            Ok(Value::Table(Table::from_entries(entries)))
        }
    }

    impl<'de> Deserialize<'de> for Value {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
            d.deserialize_any(ValueVisitor)
        }
    }

    impl<'de> Deserialize<'de> for Table {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Table, D::Error> {
            match d.deserialize_any(ValueVisitor)? {
                Value::Table(t) => Ok(t),
                other => Err(de::Error::custom(format!("a table, not {}", other.type_str()))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Str, Table, Value};

    const TEXT: &str = "z = 1\na = \"x\"\nrect = [80, 80, 8, 8]\nscale = [0.5, 2.0]\nmixed = [1, \"a\"]\n\
                        when = 1979-05-27T07:32:00Z\n[[stack]]\nop = \"file\"\n[cells.dot]\npalette = \"row_0\"\n";

    #[test]
    fn toml_reads_and_writes_back_in_order() {
        let table: Table = toml::from_str(TEXT).unwrap();
        assert_eq!(table.keys().collect::<Vec<_>>(), ["z", "a", "rect", "scale", "mixed", "when", "stack", "cells"]);
        assert!(matches!(table.get("rect"), Some(Value::Integers(items)) if **items == [80, 80, 8, 8]));
        assert!(matches!(table.get("scale"), Some(Value::Floats(_))));
        assert!(matches!(table.get("mixed"), Some(Value::Array(_))));
        assert!(matches!(table.get("when"), Some(Value::Datetime(_))));
        let written = toml::to_string(&table).unwrap();
        let again: Table = toml::from_str(&written).unwrap();
        assert_eq!(again, table);
        // As `toml` holds it, and back.
        let held: toml::Table = toml::from_str(TEXT).unwrap();
        assert_eq!(table.to_toml(), held);
        assert_eq!(Table::from(&held), table);
    }

    #[test]
    fn cbor_reads_back_as_it_was() {
        let table: Table = toml::from_str(TEXT).unwrap();
        let mut bytes = Vec::new();
        ciborium::into_writer(&table, &mut bytes).unwrap();
        let again: Table = ciborium::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(again, table);
        // The same values in the same order: the same bytes.
        let mut twice = Vec::new();
        ciborium::into_writer(&again, &mut twice).unwrap();
        assert_eq!(bytes, twice);
    }

    #[test]
    fn a_change_copies_what_it_changes_and_shares_the_rest() {
        let table: Table = toml::from_str(TEXT).unwrap();
        let mut changed = table.clone();
        assert!(Table::ptr_eq(&table, &changed));
        changed.insert("z", Value::Integer(2));
        assert!(!Table::ptr_eq(&table, &changed));
        assert_eq!(table.get("z"), Some(&Value::Integer(1)));
        // What was left be is the same, shared: its cells table.
        let (Some(Value::Table(a)), Some(Value::Table(b))) = (table.get("cells"), changed.get("cells")) else {
            panic!("tables")
        };
        assert!(Table::ptr_eq(a, b));
    }

    #[test]
    fn keys_are_kept_once_and_long_tables_found_by_index() {
        assert!(std::sync::Arc::ptr_eq(&Str::new("palette").0, &Str::new("palette").0));
        let long: Table = (0..100).map(|i| (Str::new(&format!("k{i}")), Value::Integer(i))).collect();
        assert_eq!(long.get("k57"), Some(&Value::Integer(57)));
        assert_eq!(long.get("nothing"), None);
        let mut longer = long.clone();
        longer.insert("k100", Value::Integer(100));
        assert_eq!(longer.get("k100"), Some(&Value::Integer(100)));
        assert_eq!(longer.remove("k3"), Some(Value::Integer(3)));
        assert_eq!(longer.get("k3"), None);
        assert_eq!(longer.get("k99"), Some(&Value::Integer(99)));
    }
}
