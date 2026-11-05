//! The types a package registers with the build (`PE2_TYPE`), and the values of options of them, as
//! `tools/buildsteps/option_types.py` has them: the same rules, the same text.
//!
//! A registered type is an alias of an integer primitive (`typedef s16 Angle;`), or a struct whose fields are integer primitives,
//! registered types, or fixed arrays of either, one field to a declaration; never a pointer. A primitive is one of core's fixed-size
//! integers (`s8` to `u64`, `i8` to `i64`, `byte`, the Q fixed-point types) or one of C's, whose size is the target's. An option of
//! one, or of a fixed array of one (`_LevelTuning[3]`), or of a dynamic array of one (`_Wave[]`, an option's own type alone, its
//! length what it holds and at least one), takes a C initializer as its default and a TOML value as what a package sets: a table for a
//! struct, by field, an array for an array, each merged into the default - a dynamic array's length is what the package sets, an
//! element past the default's set in full.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use pe2_ext::{Result, Value, anyhow, bail};

/// Core's fixed-size integers (`types.h`): signed or not, and bits.
const FIXED: [(&str, bool, u32); 13] = [
    ("s8", true, 8),
    ("s16", true, 16),
    ("s32", true, 32),
    ("s64", true, 64),
    ("i8", true, 8),
    ("i16", true, 16),
    ("i32", true, 32),
    ("i64", true, 64),
    ("u8", false, 8),
    ("u16", false, 16),
    ("u32", false, 32),
    ("u64", false, 64),
    ("byte", true, 8),
];

/// C's integers, whose size is the target's, by target: signed or not, and bits. psx's char is unsigned (`-funsigned-char`).
fn target_integer(target: &str, name: &str) -> Option<(bool, u32)> {
    if target != "psx" {
        return None;
    }
    Some(match name {
        "char" | "unsigned char" => (false, 8),
        "signed char" => (true, 8),
        "short" | "signed short" => (true, 16),
        "unsigned short" => (false, 16),
        "int" | "signed int" | "signed" | "long" | "signed long" => (true, 32),
        "unsigned int" | "unsigned" | "unsigned long" => (false, 32),
        "long long" | "signed long long" => (true, 64),
        "unsigned long long" => (false, 64),
        _ => return None,
    })
}

/// A Q fixed-point type, `q<integer bits>_<fraction bits>`: signed when a sign bit makes up its size.
fn fixed_point(name: &str) -> Option<(bool, u32)> {
    let (whole, fraction) = name.strip_prefix('q')?.split_once('_')?;
    let digits = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| text.parse::<u32>().ok()).flatten()
    };
    let bits = digits(whole)? + digits(fraction)?;
    if [8, 16, 32, 64].contains(&(bits + 1)) {
        Some((true, bits + 1))
    } else if [8, 16, 32, 64].contains(&bits) {
        Some((false, bits))
    } else {
        None
    }
}

/// An integer as C writes one, in decimal or hexadecimal, signed, spaces aside.
fn integer(text: &str) -> Option<i128> {
    let text: String = text.chars().filter(|c| *c != ' ').collect();
    let (negative, digits) = text.strip_prefix('-').map_or((false, text.as_str()), |rest| (true, rest));
    let magnitude = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) if !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            i128::from_str_radix(hex, 16).ok()?
        }
        None if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => digits.parse().ok()?,
        _ => return None,
    };
    Some(if negative { -magnitude } else { magnitude })
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// A type's name with its words one space apart, when it is one: a word, or C's way of naming an integer in several
/// (`unsigned long long`, `signed`).
fn type_name(words: &[&str]) -> Option<String> {
    match words {
        [one] if is_word(one) => Some((*one).to_string()),
        [] => None,
        _ => {
            let rest = match words[0] {
                "signed" | "unsigned" => &words[1..],
                _ => words,
            };
            let valid = matches!(rest, [] | ["char" | "short" | "int" | "long"] | ["long", "long"]);
            valid.then(|| words.join(" "))
        }
    }
}

/// A count in brackets at the end of `text`, and what is before it: `T[3]` is `T` and 3. None where the brackets hold no positive
/// literal.
fn counted(text: &str) -> Option<(&str, Option<usize>)> {
    let text = text.trim();
    let Some(inner) = text.strip_suffix(']') else { return Some((text, None)) };
    let (before, count) = inner.rsplit_once('[')?;
    let count = count.trim();
    if !is_word(count) {
        return None;
    }
    let count = integer(count).filter(|count| *count > 0)?;
    Some((before.trim_end(), Some(usize::try_from(count).ok()?)))
}

/// What an option or a field holds: a type, or a fixed array of it, or - an option alone - a dynamic array of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub ty: String,
    pub count: Option<usize>,
    pub dynamic: bool,
}

impl Shape {
    fn of(ty: &str, count: Option<usize>) -> Shape {
        Shape { ty: ty.to_string(), count, dynamic: false }
    }
}

/// `T`, `T[N]` (N a literal) or `T[]`.
#[must_use]
pub fn spelled(text: &str) -> Option<Shape> {
    let dynamic = text
        .trim()
        .strip_suffix(']')
        .and_then(|inner| inner.rsplit_once('['))
        .filter(|(_, count)| count.trim().is_empty());
    if let Some((before, _)) = dynamic {
        let words: Vec<&str> = before.split_whitespace().collect();
        return Some(Shape { ty: type_name(&words)?, count: None, dynamic: true });
    }
    let (before, count) = counted(text)?;
    let words: Vec<&str> = before.split_whitespace().collect();
    Some(Shape::of(&type_name(&words)?, count))
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: String,
    pub count: Option<usize>,
    /// What its comment says: a `//` after it on its line, else the `///` lines before it. For a view; no build reads it.
    pub doc: String,
}

#[derive(Clone, Debug)]
pub struct Type {
    pub at: String,
    /// An alias's integer primitive.
    pub base: Option<String>,
    pub fields: Vec<Field>,
}

/// A value of an option of a registered type or an array: an integer, a struct's fields in order, or an array's elements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tree {
    Int(i128),
    Fields(Vec<(String, Tree)>),
    List(Vec<Tree>),
}

/// The types a package's files register.
pub struct Types {
    registered: BTreeMap<String, Type>,
    wrong: Vec<String>,
    target: String,
}

/// A text with its comments blanked out, so that offsets stay.
fn uncommented(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut at = 0;
    while at < bytes.len() {
        let end = match (bytes[at], bytes.get(at + 1)) {
            (b'/', Some(b'/')) => text[at..].find('\n').map_or(bytes.len(), |end| at + end),
            (b'/', Some(b'*')) => text[at + 2..].find("*/").map_or(bytes.len(), |end| at + end + 4),
            _ => {
                at += 1;
                continue;
            }
        };
        for byte in &mut out[at..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        at = end;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Types {
    #[must_use]
    pub fn new(target: &str) -> Types {
        Types { registered: BTreeMap::new(), wrong: vec![], target: target.to_string() }
    }

    /// An integer primitive's range: core's fixed-size integers and Q types, and C's at the target's sizes.
    #[must_use]
    pub fn primitive(&self, name: &str) -> Option<(i128, i128)> {
        let (signed, bits) = FIXED
            .iter()
            .find(|(known, ..)| *known == name)
            .map(|(_, signed, bits)| (*signed, *bits))
            .or_else(|| fixed_point(name))
            .or_else(|| target_integer(&self.target, name))?;
        Some(if signed { (-(1 << (bits - 1)), (1 << (bits - 1)) - 1) } else { (0, (1 << bits) - 1) })
    }

    /// A `PE2_TYPE(name)` at `offset` of `text`: the typedef of that name before it, in the same file.
    pub fn register(&mut self, name: &str, text: &str, offset: usize, at: &str) {
        if let Some(known) = self.registered.get(name) {
            self.wrong.push(format!("{at}: {name} is registered already, at {}", known.at));
            return;
        }
        let before = uncommented(&text[..offset.min(text.len())]);
        let (alias, closing) = ends_of(&before, name);
        if let Some((_, base)) = alias.filter(|(start, _)| closing.is_none_or(|closing| *start > closing)) {
            let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
            if base.contains('*') {
                self.wrong.push(format!("{at}: {name} is a pointer, which no registered type is"));
            } else if self.primitive(&base).is_none() {
                self.wrong.push(format!(
                    "{at}: {name} is an alias of {base}, and a registered alias is of an integer primitive"
                ));
            } else {
                self.registered.insert(name.to_string(), Type { at: at.to_string(), base: Some(base), fields: vec![] });
            }
            return;
        }
        let Some(end) = closing else {
            self.wrong.push(format!("{at}: PE2_TYPE({name}) follows no typedef of {name} in its file"));
            return;
        };
        let bytes = before.as_bytes();
        let (mut depth, mut start) = (0, end);
        while start > 0 {
            start -= 1;
            match bytes[start] {
                b'}' => depth += 1,
                b'{' if depth == 0 => break,
                b'{' => depth -= 1,
                _ => {}
            }
        }
        if !is_struct_head(&before[..start]) {
            self.wrong.push(format!(
                "{at}: {name} is registered as a struct or an alias of a fixed-size integer, and is neither"
            ));
            return;
        }
        let mut fields = vec![];
        let mut cursor = start + 1;
        for declaration in before[start + 1..end].split(';') {
            let (from, to) = (cursor, cursor + declaration.len());
            cursor = to + 1;
            let doc = doc_of(text, from, to);
            let declaration = declaration.trim();
            if declaration.is_empty() {
                continue;
            }
            if declaration.contains('*') {
                self.wrong.push(format!(
                    "{at}: {name}: `{declaration}` is a pointer, which no field of a registered struct is"
                ));
                return;
            }
            let field = counted(declaration).and_then(|(before, count)| {
                let words: Vec<&str> = before.split_whitespace().collect();
                let (field, ty) = words.split_last()?;
                (is_word(field)).then_some(())?;
                Some(Field { name: (*field).to_string(), ty: type_name(ty)?, count, doc: doc.clone() })
            });
            let Some(field) = field else {
                self.wrong.push(format!(
                    "{at}: {name}: `{declaration}` is no field of a registered struct: one integer or registered type, or a fixed array of one, to a declaration"
                ));
                return;
            };
            fields.push(field);
        }
        self.registered.insert(name.to_string(), Type { at: at.to_string(), base: None, fields });
    }

    /// The first thing wrong with what was registered: a field of a type that is neither an integer primitive nor registered (C has its
    /// fields defined before it, so no type can hold itself).
    pub fn check(&self) -> Result<()> {
        if let Some(wrong) = self.wrong.first() {
            bail!("{wrong}");
        }
        for (name, registered) in &self.registered {
            for field in &registered.fields {
                if self.primitive(&field.ty).is_none() && !self.registered.contains_key(&field.ty) {
                    bail!(
                        "{}: {name}.{} is a {}, which is neither an integer primitive of {} nor a registered type",
                        registered.at,
                        field.name,
                        field.ty,
                        self.target
                    );
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn knows(&self, shape: &Shape) -> bool {
        self.primitive(&shape.ty).is_some() || self.registered.contains_key(&shape.ty)
    }

    /// What a view shows of a shape: its type, its count if an array, and an integer's range or a struct's fields, each with its own.
    #[must_use]
    pub fn describe(&self, shape: &Shape) -> Value {
        let mut out = pe2_ext::Table::new();
        out.insert("type".into(), shape.ty.clone().into());
        if let Some(count) = shape.count {
            out.insert("count".into(), i64::try_from(count).unwrap_or(i64::MAX).into());
        }
        if shape.dynamic {
            out.insert("dynamic".into(), true.into());
        }
        if let Some((low, high)) = self.integral(&shape.ty) {
            // A range past what TOML holds is cut to it: what a package can set is a TOML integer.
            let clamp = |number: i128| i64::try_from(number).unwrap_or(if number < 0 { i64::MIN } else { i64::MAX });
            out.insert("low".into(), clamp(low).into());
            out.insert("high".into(), clamp(high).into());
        } else if let Some(registered) = self.registered.get(&shape.ty) {
            let fields = registered.fields.iter().map(|field| {
                let Value::Table(mut described) = self.describe(&Shape::of(&field.ty, field.count)) else {
                    unreachable!()
                };
                described.insert("name".into(), field.name.clone().into());
                if !field.doc.is_empty() {
                    described.insert("doc".into(), field.doc.clone().into());
                }
                Value::Table(described)
            });
            out.insert("fields".into(), Value::Array(fields.collect()));
        }
        Value::Table(out)
    }

    /// The range of a primitive or a registered alias; none for a struct.
    #[must_use]
    pub fn integral(&self, name: &str) -> Option<(i128, i128)> {
        match self.registered.get(name) {
            Some(registered) => registered.base.as_deref().and_then(|base| self.primitive(base)),
            None => self.primitive(name),
        }
    }

    fn ranged(&self, name: &str, value: Option<i128>, shown: &str, place: &str) -> Result<Tree> {
        let (low, high) = self.integral(name).ok_or_else(|| anyhow!("{place} is a {name}, which holds no integer"))?;
        match value {
            Some(value) if (low..=high).contains(&value) => Ok(Tree::Int(value)),
            _ => bail!("{place} is a {name}, {low} to {high}, and is {shown}"),
        }
    }

    /// A default as C writes it, held to its shape.
    pub fn parsed(&self, shape: &Shape, written: &str, at: &str) -> Result<Tree> {
        let tokens = initializer_tokens(written);
        let mut position = 0;
        let parsed = tree_of(&tokens, &mut position, at)?;
        if position != tokens.len() {
            bail!("{at}: the default ends before `{}`", tokens[position..].concat());
        }
        self.shaped(shape, &parsed, at, "")
    }

    fn shaped(&self, shape: &Shape, value: &Written, at: &str, path: &str) -> Result<Tree> {
        let place = format!("{at}: {}", if path.is_empty() { "the value" } else { path });
        if shape.dynamic {
            let Written::List(items) = value else { bail!("{place} is one or more of {}", shape.ty) };
            if items.is_empty() {
                bail!("{place} is one or more of {}", shape.ty);
            }
            let one = Shape::of(&shape.ty, None);
            return items
                .iter()
                .enumerate()
                .map(|(index, item)| self.shaped(&one, item, at, &format!("{path}[{index}]")))
                .collect::<Result<_>>()
                .map(Tree::List);
        }
        if let Some(count) = shape.count {
            let Written::List(items) = value else { bail!("{place} is {count} of {}", shape.ty) };
            if items.len() != count {
                bail!("{place} is {count} of {}", shape.ty);
            }
            let one = Shape::of(&shape.ty, None);
            return items
                .iter()
                .enumerate()
                .map(|(index, item)| self.shaped(&one, item, at, &format!("{path}[{index}]")))
                .collect::<Result<_>>()
                .map(Tree::List);
        }
        if self.integral(&shape.ty).is_some() {
            return match value {
                Written::Int(number) => self.ranged(&shape.ty, Some(*number), &number.to_string(), &place),
                Written::List(_) => self.ranged(&shape.ty, None, "a list", &place),
            };
        }
        let fields = &self.registered[&shape.ty].fields;
        let Written::List(items) = value else { bail!("{place} is a {}: {} fields, in order", shape.ty, fields.len()) };
        if items.len() != fields.len() {
            bail!("{place} is a {}: {} fields, in order", shape.ty, fields.len());
        }
        let mut out = vec![];
        for (field, item) in fields.iter().zip(items) {
            let inner = Shape::of(&field.ty, field.count);
            let path = if path.is_empty() { field.name.clone() } else { format!("{path}.{}", field.name) };
            out.push((field.name.clone(), self.shaped(&inner, item, at, &path)?));
        }
        Ok(Tree::Fields(out))
    }

    /// What a package sets merged into the default: a struct's fields it names, an array's elements, an integer. With no default, every
    /// field is set.
    pub fn merged(&self, shape: &Shape, default: Option<&Tree>, given: &Value, at: &str, path: &str) -> Result<Tree> {
        let place = format!("{at}: {}", if path.is_empty() { "the value" } else { path });
        if shape.dynamic {
            let items = given
                .as_array()
                .filter(|items| !items.is_empty())
                .ok_or_else(|| anyhow!("{place} is set to one or more of {}", shape.ty))?;
            let one = Shape::of(&shape.ty, None);
            let known: Vec<&Tree> = match default {
                Some(Tree::List(list)) => list.iter().collect(),
                _ => vec![],
            };
            let merged = items.iter().enumerate().map(|(index, item)| {
                self.merged(&one, known.get(index).copied(), item, at, &format!("{path}[{index}]"))
            });
            return merged.collect::<Result<_>>().map(Tree::List);
        }
        if let Some(count) = shape.count {
            let items = given
                .as_array()
                .filter(|items| items.len() == count)
                .ok_or_else(|| anyhow!("{place} is set to {count} of {}", shape.ty))?;
            let one = Shape::of(&shape.ty, None);
            let defaults: Vec<Option<&Tree>> = match default {
                Some(Tree::List(list)) => list.iter().map(Some).collect(),
                _ => vec![None; count],
            };
            let merged = items
                .iter()
                .zip(defaults)
                .enumerate()
                .map(|(index, (item, old))| self.merged(&one, old, item, at, &format!("{path}[{index}]")));
            return merged.collect::<Result<_>>().map(Tree::List);
        }
        if self.integral(&shape.ty).is_some() {
            let number = given.as_integer().map(i128::from);
            return self.ranged(&shape.ty, number, &given.to_string(), &place);
        }
        let Some(table) = given.as_table() else { bail!("{place} is a {}, set by a table of its fields", shape.ty) };
        let fields = &self.registered[&shape.ty].fields;
        if let Some(name) = table.keys().find(|name| !fields.iter().any(|field| &field.name == *name)) {
            bail!("{place}: a {} has no field {name}", shape.ty);
        }
        if default.is_none() && table.len() != fields.len() {
            let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
            bail!("{place} has no default, so all of a {}'s fields are set: {}", shape.ty, names.join(", "));
        }
        let old: Vec<(String, Tree)> = match default {
            Some(Tree::Fields(old)) => old.clone(),
            _ => vec![],
        };
        let mut out = vec![];
        for field in fields {
            let inner = Shape::of(&field.ty, field.count);
            let path = if path.is_empty() { field.name.clone() } else { format!("{path}.{}", field.name) };
            let before = old.iter().find(|(name, _)| *name == field.name).map(|(_, tree)| tree);
            let value = match table.get(&field.name) {
                Some(given) => self.merged(&inner, before, given, at, &path)?,
                None => before.cloned().ok_or_else(|| anyhow!("{place}: {} is not set", field.name))?,
            };
            out.push((field.name.clone(), value));
        }
        Ok(Tree::Fields(out))
    }
}

/// Where the typedef of `name` ends before a text's end: the last `typedef BASE name;`, with where it starts and its base, and the
/// last `} name;`, where its brace is.
fn ends_of(before: &str, name: &str) -> (Option<(usize, String)>, Option<usize>) {
    let (mut alias, mut closing) = (None, None);
    let mut from = 0;
    while let Some(found) = before[from..].find(name) {
        let start = from + found;
        from = start + name.len();
        let bounded = (start == 0 || !is_word(&before[start - 1..start]))
            && !before[from..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if !bounded || !before[from..].trim_start().starts_with(';') {
            continue;
        }
        let head = before[..start].trim_end();
        if head.ends_with('}') {
            closing = Some(head.len() - 1);
            continue;
        }
        let statement = head.rfind([';', '{', '}']).map_or(0, |at| at + 1);
        let text = &before[statement..start];
        let trimmed = text.trim_start();
        if let Some(base) = trimmed.strip_prefix("typedef").filter(|rest| rest.starts_with(char::is_whitespace)) {
            let base = base.trim();
            if !base.is_empty() && !base.contains(['{', '}']) {
                alias = Some((statement + (text.len() - trimmed.len()), base.to_string()));
            }
        }
    }
    (alias, closing)
}

/// A field's comment, from the original text: a `//` after its `;` on that line, else the `///` lines just before it, in the stretch
/// `from..to` its declaration takes.
fn doc_of(text: &str, from: usize, to: usize) -> String {
    let after = &text[(to + 1).min(text.len())..];
    let line = after.split('\n').next().unwrap_or_default().trim();
    if let Some(comment) = line.strip_prefix("//") {
        return comment.trim_start_matches('/').trim().to_string();
    }
    let lines: Vec<&str> = text[from.min(text.len())..to.min(text.len())].lines().map(str::trim).collect();
    let code = lines.iter().position(|line| !line.is_empty() && !line.starts_with("//")).unwrap_or(lines.len());
    let above = lines[..code]
        .iter()
        .rev()
        .take_while(|line| line.starts_with("///"))
        .map(|line| line.trim_start_matches('/').trim())
        .collect::<Vec<_>>();
    above.into_iter().rev().collect::<Vec<_>>().join(" ")
}

/// Whether a text ends in `typedef struct`, with or without a tag.
fn is_struct_head(text: &str) -> bool {
    let statement = text.rfind([';', '{', '}']).map_or(text, |at| &text[at + 1..]);
    matches!(
        statement.split_whitespace().collect::<Vec<_>>().as_slice(),
        ["typedef", "struct"] | ["typedef", "struct", _]
    )
}

/// What a C initializer is written as, before its shape is known.
enum Written {
    Int(i128),
    List(Vec<Written>),
}

/// An initializer's tokens: braces, commas, and what is between them.
fn initializer_tokens(written: &str) -> Vec<String> {
    let mut tokens = vec![];
    let mut current = String::new();
    for c in written.chars() {
        if matches!(c, '{' | '}' | ',') {
            if !current.trim().is_empty() {
                tokens.push(current.trim_end().to_string());
            }
            current.clear();
            tokens.push(c.to_string());
        } else if !(current.is_empty() && c.is_whitespace()) {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim_end().to_string());
    }
    tokens
}

fn tree_of(tokens: &[String], position: &mut usize, at: &str) -> Result<Written> {
    if tokens.get(*position).is_some_and(|token| token == "{") {
        *position += 1;
        let mut items = vec![];
        while tokens.get(*position).is_some_and(|token| token != "}") {
            items.push(tree_of(tokens, position, at)?);
            if tokens.get(*position).is_some_and(|token| token == ",") {
                *position += 1;
            }
        }
        *position += 1;
        return Ok(Written::List(items));
    }
    let token = tokens.get(*position).map_or("", |token| token.trim());
    *position += 1;
    integer(token).map(Written::Int).ok_or_else(|| anyhow!("{at}: `{token}` is no integer literal"))
}

impl Tree {
    /// A value as a package's declaration sets it: an integer, a table by field, an array.
    #[must_use]
    pub fn value(&self) -> Value {
        match self {
            Tree::Int(number) => {
                i64::try_from(*number).map_or_else(|_| Value::String(number.to_string()), Value::Integer)
            }
            Tree::Fields(fields) => {
                Value::Table(fields.iter().map(|(name, tree)| (name.clone(), tree.value())).collect())
            }
            Tree::List(items) => Value::Array(items.iter().map(Tree::value).collect()),
        }
    }

    /// A value as C initializes it.
    #[must_use]
    pub fn initializer(&self) -> String {
        match self {
            Tree::Int(number) => number.to_string(),
            Tree::Fields(fields) => {
                format!("{{ {} }}", fields.iter().map(|(_, tree)| tree.initializer()).collect::<Vec<_>>().join(", "))
            }
            Tree::List(items) => {
                format!("{{ {} }}", items.iter().map(Tree::initializer).collect::<Vec<_>>().join(", "))
            }
        }
    }

    /// Each integer of a value, by its path (`PE2_OPTION_AT`'s arguments after the name).
    #[must_use]
    pub fn leaves(&self) -> Vec<(Vec<String>, i128)> {
        let mut out = vec![];
        self.leaves_into(&mut vec![], &mut out);
        out
    }

    fn leaves_into(&self, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, i128)>) {
        match self {
            Tree::Int(number) => out.push((path.clone(), *number)),
            Tree::Fields(fields) => {
                for (name, tree) in fields {
                    path.push(name.clone());
                    tree.leaves_into(path, out);
                    path.pop();
                }
            }
            Tree::List(items) => {
                for (index, tree) in items.iter().enumerate() {
                    path.push(index.to_string());
                    tree.leaves_into(path, out);
                    path.pop();
                }
            }
        }
    }

    /// The macros `pe2_options.h` defines for each integer of an option's value.
    #[must_use]
    pub fn leaf_macros(&self, name: &str) -> String {
        let mut out = String::new();
        for (path, number) in self.leaves().into_iter().filter(|(path, _)| !path.is_empty()) {
            let _ = writeln!(out, "#define pe2_option_{name}_{} {number}", path.join("_"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPE: &str = "typedef struct {\n    s16 wedges; // Fanned\n    unsigned short limit;\n    u8 steps[2];\n} _Level;\nPE2_TYPE(_Level)\n";

    fn registered() -> Types {
        let mut types = Types::new("psx");
        types.register("_Level", TYPE, TYPE.find("PE2_TYPE").unwrap(), "a.c:6");
        types
    }

    #[test]
    fn a_struct_registers_its_fields_and_an_alias_its_primitive() {
        let mut types = registered();
        let alias = "typedef unsigned  int Angle;\n";
        types.register("Angle", alias, alias.len(), "a.c:1");
        types.check().unwrap();
        let fields: Vec<(&str, &str, Option<usize>)> =
            types.registered["_Level"].fields.iter().map(|f| (f.name.as_str(), f.ty.as_str(), f.count)).collect();
        assert_eq!(fields, [("wedges", "s16", None), ("limit", "unsigned short", None), ("steps", "u8", Some(2))]);
        assert_eq!(types.registered["Angle"].base.as_deref(), Some("unsigned int"));
        assert_eq!(types.registered["_Level"].fields[0].doc, "Fanned");
        assert_eq!(types.primitive("q19_12"), Some((-(1 << 31), (1 << 31) - 1)));
        assert_eq!(types.primitive("q8_8"), Some((0, 0xFFFF)));
        assert_eq!(types.primitive("char"), Some((0, 0xFF)));
    }

    #[test]
    fn a_value_reads_merges_and_writes_as_the_build_does() {
        let types = registered();
        let shape = spelled("_Level[2]").unwrap();
        let default = types.parsed(&shape, "{ { 8, 0x90, { 1, 2 } }, { -1, 0, { 3, 4 } } }", "a.c:7").unwrap();
        let given: Value = toml::from_str::<toml::Table>("v = [{}, { limit = 7 }]").unwrap()["v"].clone();
        let merged = types.merged(&shape, Some(&default), &given, "decl", "").unwrap();
        assert_eq!(merged.initializer(), "{ { 8, 144, { 1, 2 } }, { -1, 7, { 3, 4 } } }");
        assert!(merged.leaf_macros("LEVELS").contains("#define pe2_option_LEVELS_1_steps_0 3\n"));
    }

    #[test]
    fn a_dynamic_array_is_as_long_as_what_is_set() {
        let types = registered();
        let shape = spelled("_Level[]").unwrap();
        assert!(shape.dynamic && shape.count.is_none());
        let default = types.parsed(&shape, "{ { 1, 0, { 2, 3 } }, { 4, 0, { 5, 6 } } }", "a.c:7").unwrap();
        let given: Value =
            toml::from_str::<toml::Table>("v = [{ wedges = 9 }, {}, { wedges = 7, limit = 1, steps = [8, 9] }]")
                .unwrap()["v"]
                .clone();
        let merged = types.merged(&shape, Some(&default), &given, "decl", "").unwrap();
        assert_eq!(merged.initializer(), "{ { 9, 0, { 2, 3 } }, { 4, 0, { 5, 6 } }, { 7, 1, { 8, 9 } } }");
        let partial: Value = toml::from_str::<toml::Table>("v = [{}, {}, { wedges = 7 }]").unwrap()["v"].clone();
        let wrong = types.merged(&shape, Some(&default), &partial, "decl", "").unwrap_err().to_string();
        assert_eq!(wrong, "decl: [2] has no default, so all of a _Level's fields are set: wedges, limit, steps");
        let empty: Value = toml::from_str::<toml::Table>("v = []").unwrap()["v"].clone();
        let wrong = types.merged(&shape, Some(&default), &empty, "decl", "").unwrap_err().to_string();
        assert_eq!(wrong, "decl: the value is set to one or more of _Level");
    }

    #[test]
    fn what_is_wrong_is_said_as_the_build_says_it() {
        let wrong = |text: &str, name: &str| {
            let mut types = Types::new("psx");
            types.register(name, text, text.find("PE2_TYPE").unwrap_or(text.len()), "a.c:2");
            types.check().unwrap_err().to_string()
        };
        assert_eq!(
            wrong("typedef struct { s16* p; } P;\nPE2_TYPE(P)\n", "P"),
            "a.c:2: P: `s16* p` is a pointer, which no field of a registered struct is"
        );
        assert_eq!(
            wrong("typedef struct { float f; } F;\nPE2_TYPE(F)\n", "F"),
            "a.c:2: F.f is a float, which is neither an integer primitive of psx nor a registered type"
        );
        assert_eq!(wrong("PE2_TYPE(G)\n", "G"), "a.c:2: PE2_TYPE(G) follows no typedef of G in its file");
        let types = registered();
        let short = types.parsed(&spelled("_Level").unwrap(), "{ 1, 2 }", "a.c:7").unwrap_err().to_string();
        assert_eq!(short, "a.c:7: the value is a _Level: 3 fields, in order");
        let big = types.parsed(&spelled("s16").unwrap(), "40000", "a.c:1: Y").unwrap_err().to_string();
        assert_eq!(big, "a.c:1: Y: the value is a s16, -32768 to 32767, and is 40000");
    }
}
