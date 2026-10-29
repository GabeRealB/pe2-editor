//! The work graph as an extension sees it: values and their types, the operations it registers, and the outputs its
//! kinds declare.
//!
//! Every value carries its type. The host's own types it shows itself - an RGBA image, a table, PCM audio, a list,
//! `bytes` (what nothing recognizes) - and an extension's types are named (`pe2.images@indexed`) and opaque to the
//! host: their encoding, a set of files with the settings derived with them, moved and kept as it is and read only by
//! what takes it. A type may be recognized in a stored file (`TypeDef`); a value recognized as several things answers
//! to each (`pe2.formats@ora`, `pe2.formats@zip`, `image/openraster`).
//!
//! Where a value lives is no part of its type: a file's content is inline or stored - a blob by its hash, which the
//! host reads on asking, whole or by range - so a large file passes between operations by reference, and a port asking
//! for a file (`file<T>`, `file<any>`) asks for a representation - a value that is one file - never another type.
//!
//! An operation's ports are typed: by a type, a union (`a | b`), a variable bound across its ports and its output
//! (`T`: `select(a: T, b: T) -> T`), or `any`, the operation then dispatching on what it is given. The host checks what
//! it can when a stack is lowered, and the rest when the operation runs.

use std::fmt;
use std::sync::Arc;

use crate::{Asset, Field, Image, Name, Result, Source, Table, anyhow, bail};

/// A type: of a port, an output, or a value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// Whatever it is given.
    Any,
    /// A variable (`T`): what it is bound to by the first port given a value, the same for the others and the output.
    Var(String),
    Image,
    Table,
    Pcm,
    /// Data nothing recognizes, as files.
    Bytes,
    List(Box<Type>),
    /// An extension's type, qualified, or a media type (`image/openraster`).
    Named(String),
    /// Any of these.
    Union(Vec<Type>),
    /// Of a type, as one file: a representation, which the host makes a value of the type take where it can.
    File(Box<Type>),
}

impl Type {
    /// A type as it is written: `any`, `image`, `table`, `pcm`, `bytes`, a variable (one capital letter, `T`),
    /// `list<T>`, `file<T>` (`file` alone: `file<any>`), a union (`a | b`), or a type's name - bare, qualified by `id`
    /// - or a media type.
    ///
    /// # Errors
    /// It is none of these.
    pub fn parse(text: &str, id: &str) -> Result<Type> {
        let text = text.trim();
        // A union: split where `|` is outside `<...>`.
        let mut depth = 0i32;
        let mut parts = vec![];
        let mut start = 0;
        for (i, c) in text.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                '|' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        if !parts.is_empty() {
            parts.push(&text[start..]);
            return Ok(Type::Union(parts.into_iter().map(|p| Type::parse(p, id)).collect::<Result<_>>()?));
        }
        let inner = |prefix: &str| text.strip_prefix(prefix).and_then(|t| t.strip_suffix('>'));
        Ok(match text {
            "any" => Type::Any,
            "image" => Type::Image,
            "table" => Type::Table,
            "pcm" => Type::Pcm,
            "bytes" => Type::Bytes,
            "file" => Type::File(Box::new(Type::Any)),
            _ if inner("file<").is_some() => Type::File(Box::new(Type::parse(inner("file<").unwrap_or_default(), id)?)),
            _ if inner("list<").is_some() => Type::List(Box::new(Type::parse(inner("list<").unwrap_or_default(), id)?)),
            _ if text.len() == 1 && text.chars().all(|c| c.is_ascii_uppercase()) => Type::Var(text.to_string()),
            _ if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "_.@-/+".contains(c)) => {
                Type::Named(qualify(text, id))
            }
            _ => bail!("`{text}` is not a type"),
        })
    }

    /// Whether a value of type `given` may be given to a port of this type, binding its variables. A union fits
    /// where any of it does; a value answering to several names fits a port naming one. A file is no other type:
    /// `file<T>` takes a `T` as files (that it is one is checked when it is given) - never an image, a table, audio
    /// or a list, which the host holds in memory alone - and a port of `T` a file of one.
    pub fn unify(&self, given: &Type, bound: &mut Vec<(String, Type)>) -> bool {
        match (self, given) {
            // Anything fits `any`; a variable not yet known, anything; bytes are what any type is encoded as.
            (Type::Any, _) | (_, Type::Any | Type::Var(_)) | (Type::Bytes, Type::Named(_)) => true,
            (Type::Var(v), given) => match bound.iter().find(|(b, _)| b == v).map(|(_, t)| t.clone()) {
                Some(Type::Var(_)) | None => {
                    bound.retain(|(b, _)| b != v);
                    bound.push((v.clone(), given.clone()));
                    true
                }
                Some(bound_to) => bound_to.unify(given, &mut Vec::new()),
            },
            (Type::Union(ports), given) => ports.iter().any(|p| {
                let mut tried = bound.clone();
                let fits = p.unify(given, &mut tried);
                if fits {
                    *bound = tried;
                }
                fits
            }),
            (port, Type::Union(given)) => given.iter().any(|g| port.unify(g, bound)),
            (Type::File(port), Type::File(given)) => port.unify(given, bound),
            // What the host holds in memory alone is no file.
            (Type::File(_), Type::Image | Type::Table | Type::Pcm | Type::List(_)) => false,
            (Type::File(port), given) => port.unify(given, bound),
            (port, Type::File(given)) => port.unify(given.as_ref(), bound),
            (Type::List(a), Type::List(b)) => a.unify(b, bound),
            (a, b) => a == b,
        }
    }

    /// This type with its variables as bound.
    #[must_use]
    pub fn bind(&self, bound: &[(String, Type)]) -> Type {
        match self {
            Type::Var(v) => bound.iter().find(|(b, _)| b == v).map_or_else(|| self.clone(), |(_, t)| t.clone()),
            Type::List(item) => Type::List(Box::new(item.bind(bound))),
            Type::File(of) => Type::File(Box::new(of.bind(bound))),
            Type::Union(any) => Type::Union(any.iter().map(|t| t.bind(bound)).collect()),
            _ => self.clone(),
        }
    }

    /// Whether a port of it asks for a file.
    #[must_use]
    pub fn wants_file(&self) -> bool {
        matches!(self, Type::File(_))
    }
}

/// A bare name qualified by an extension's id; a qualified one, or a media type, as it is.
fn qualify(name: &str, id: &str) -> String {
    if name.contains(['@', '/']) || id.is_empty() { name.to_string() } else { format!("{id}@{name}") }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Any => f.write_str("any"),
            Type::Var(v) => f.write_str(v),
            Type::File(of) if **of == Type::Any => f.write_str("file"),
            Type::File(of) => write!(f, "file<{of}>"),
            Type::Image => f.write_str("image"),
            Type::Table => f.write_str("table"),
            Type::Pcm => f.write_str("pcm"),
            Type::Bytes => f.write_str("bytes"),
            Type::List(item) => write!(f, "list<{item}>"),
            Type::Named(name) => f.write_str(name),
            Type::Union(any) => {
                let names: Vec<String> = any.iter().map(ToString::to_string).collect();
                f.write_str(&names.join(" | "))
            }
        }
    }
}

/// A file's content: its bytes, or a blob the host keeps, by its hash, read on asking (`Call::read`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Inline(Vec<u8>),
    Stored { sha1: String, size: u64 },
}

impl Content {
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Content::Inline(bytes) => bytes.len() as u64,
            Content::Stored { size, .. } => *size,
        }
    }
}

/// A file of a value's encoding: its name, its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub content: Content,
}

impl File {
    /// A file made now, its bytes inline.
    #[must_use]
    pub fn new(name: &str, bytes: Vec<u8>) -> File {
        File { name: name.to_string(), content: Content::Inline(bytes) }
    }
}

/// Audio: interleaved 16-bit samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// A value of a type as files - an extension's, or `bytes`: its encoding - its files - the other names it answers to
/// (what else it was recognized as, media types), and what was derived with it: settings that take the place of its
/// node's declared ones (an image's size, its cells' rectangles), and notes on what was taken otherwise than given.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Typed {
    pub ty: String,
    pub also: Vec<String>,
    pub files: Vec<File>,
    pub settings: Table,
    pub notes: Vec<String>,
}

/// What nothing recognizes: `bytes`.
pub const BYTES: &str = "bytes";

impl Typed {
    /// A value of a type, as files made now.
    #[must_use]
    pub fn new(ty: &str, files: Vec<File>) -> Typed {
        Typed { ty: ty.to_string(), files, ..Typed::default() }
    }

    /// Its one file.
    ///
    /// # Errors
    /// It is no single file.
    pub fn file(&self) -> Result<&File> {
        match self.files.as_slice() {
            [file] => Ok(file),
            files => bail!("{} is {} files, not one", self.ty, files.len()),
        }
    }
}

/// A value, with its type.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Image(Image),
    Table(Table),
    Pcm(Pcm),
    List(Vec<Value>),
    Typed(Typed),
}

impl Value {
    #[must_use]
    pub fn ty(&self) -> Type {
        match self {
            Value::Image(_) => Type::Image,
            Value::Table(_) => Type::Table,
            Value::Pcm(_) => Type::Pcm,
            Value::List(items) => Type::List(Box::new(items.first().map_or(Type::Any, Value::ty))),
            Value::Typed(typed) => {
                let named = |name: &str| if name == BYTES { Type::Bytes } else { Type::Named(name.to_string()) };
                let ty = if typed.also.is_empty() {
                    named(&typed.ty)
                } else {
                    Type::Union(std::iter::once(&typed.ty).chain(&typed.also).map(|n| named(n)).collect())
                };
                if typed.files.len() == 1 { Type::File(Box::new(ty)) } else { ty }
            }
        }
    }

    /// The value as one of an extension's types.
    ///
    /// # Errors
    /// It is not one.
    pub fn into_typed(self) -> Result<Typed> {
        match self {
            Value::Typed(typed) => Ok(typed),
            other => bail!("given {}, not one of an extension's types", other.ty()),
        }
    }
}

/// A port of an operation: what it takes, and whether it takes any number of values (a list, in order).
#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub ty: Type,
    pub many: bool,
}

/// How an output is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retain {
    /// In the project's store, by content.
    Cache,
    /// In the project's build directory.
    Build,
    /// Not at all: handed to whoever asked, what it read kept.
    Transient,
}

/// What an operation is lent of its node and the host while it runs.
pub trait CallHost {
    /// Its node's path.
    fn path(&self) -> &str;
    /// Its node's kind and role, qualified.
    fn kind(&self) -> Option<&str>;
    fn role(&self) -> Option<&str>;
    /// Its node's settings, as declared: recorded as read.
    fn fields(&self) -> &Table;
    /// A content, whole.
    ///
    /// # Errors
    /// It is stored, and the host has no such blob or cannot read it.
    fn read(&self, content: &Content) -> Result<Vec<u8>>;
    /// Up to `len` bytes of a content from `offset`.
    ///
    /// # Errors
    /// As `read`.
    fn read_range(&self, content: &Content, offset: u64, len: usize) -> Result<Vec<u8>>;
}

/// An operation running: the values it is given by port, its settings, the value of its parameter as its output's
/// address gives it (`night` of `picture/night`; read by `parameter`), and its host.
pub struct Call<'a> {
    pub inputs: Vec<Vec<Value>>,
    pub settings: &'a Table,
    pub parameter_text: Option<&'a str>,
    pub host: &'a dyn CallHost,
}

impl Call<'_> {
    /// Its parameter's value, as its type's (`call.parameter::<String>()`).
    ///
    /// # Errors
    /// It is given none, or one its type does not read.
    pub fn parameter<P: crate::ty::Parameter>(&self) -> Result<P> {
        let text = self.parameter_text.ok_or_else(|| anyhow!("given no parameter"))?;
        P::from_text(text)
    }

    /// The value given to a port taking one, as its type's (`call.take::<Indexed>(0)`; `ty::Any`, as given).
    ///
    /// # Errors
    /// It was given none, or one not of the type.
    pub fn take<T: crate::ty::ValueType>(&mut self, port: usize) -> Result<T::Value> {
        let data = self.inputs.get_mut(port).and_then(Vec::pop).ok_or_else(|| anyhow!("its port {port} was given nothing"))?;
        T::decode(data, self)
    }

    /// The values given to a port taking any number, in order, as their type's.
    ///
    /// # Errors
    /// One is not of the type.
    pub fn take_all<T: crate::ty::ValueType>(&mut self, port: usize) -> Result<Vec<T::Value>> {
        let given = self.inputs.get_mut(port).map_or_else(Vec::new, std::mem::take);
        given.into_iter().map(|data| T::decode(data, self)).collect()
    }

    /// A file's content, whole.
    ///
    /// # Errors
    /// It cannot be read.
    pub fn read(&self, file: &File) -> Result<Vec<u8>> {
        match &file.content {
            Content::Inline(bytes) => Ok(bytes.clone()),
            stored @ Content::Stored { .. } => self.host.read(stored),
        }
    }

    /// Lend a value of an extension's type as an asset: its files, and the node's settings with what was derived with
    /// it in their places - what reads an asset reads them.
    #[must_use]
    pub fn lend<'b>(&'b self, typed: &'b Typed) -> Lent<'b> {
        Lent { host: self.host, files: &typed.files, fields: merged(self.host.fields(), &typed.settings) }
    }

    /// Lend the node's settings with others in their places - what was derived with a value decoded - as an asset
    /// of no files.
    #[must_use]
    pub fn lend_settings(&self, derived: &Table) -> Lent<'_> {
        Lent { host: self.host, files: &[], fields: merged(self.host.fields(), derived) }
    }

    /// Lend files as an asset, with the node's settings.
    #[must_use]
    pub fn lend_files<'b>(&'b self, files: &'b [File]) -> Lent<'b> {
        Lent { host: self.host, files, fields: self.host.fields().clone() }
    }
}

/// Settings with others in their places: a table merged into a table, key by key; anything else replaced.
#[must_use]
pub fn merged(declared: &Table, derived: &Table) -> Table {
    let mut out = declared.clone();
    for (key, value) in derived {
        match (out.get_mut(key), value) {
            (Some(crate::Value::Table(into)), crate::Value::Table(from)) => *into = merged(into, from),
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// Files lent as an asset (`Call::lend`): the node's path, settings as lent, the files' contents read through the host.
pub struct Lent<'a> {
    host: &'a dyn CallHost,
    files: &'a [File],
    fields: Table,
}

impl<'a> Lent<'a> {
    /// As an asset of its node's kind and role.
    #[must_use]
    pub fn asset(&'a self) -> Asset<'a> {
        Asset::new(self, self.host.kind(), self.host.role())
    }

    fn file(&self, name: &str) -> Result<&File> {
        self.files.iter().find(|f| f.name == name).ok_or_else(|| anyhow!("{}: no file {name}", self.host.path()))
    }
}

impl Source for Lent<'_> {
    fn path(&self) -> &str {
        self.host.path()
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn fields(&self) -> &Table {
        &self.fields
    }

    fn declaration(&self) -> &'static str {
        ""
    }

    fn has_file(&self, name: &str) -> bool {
        self.files.iter().any(|f| f.name == name)
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        match &self.file(name)?.content {
            Content::Inline(bytes) => Ok(bytes.clone()),
            stored @ Content::Stored { .. } => self.host.read(stored),
        }
    }

    fn size(&self, name: &str) -> Option<u64> {
        Some(self.file(name).ok()?.content.size())
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        match &self.file(name)?.content {
            Content::Inline(bytes) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
            }
            stored @ Content::Stored { .. } => self.host.read_range(stored, offset, len),
        }
    }
}

/// What an operation does when it runs.
pub type RunOp = dyn Fn(&mut Call<'_>) -> Result<Value> + Send + Sync;

/// An operation as registered.
#[derive(Clone)]
pub struct OpDef {
    pub name: Name,
    pub label: String,
    pub inputs: Vec<Port>,
    pub output: Type,
    /// Its settings, as the stack view shows them.
    pub fields: Vec<Field>,
    /// The settings of its node it derives: what it makes says them, in the declaration's place.
    pub derives: Vec<String>,
    /// The operation undoing it, qualified: what writes its input from what it makes.
    pub inverse: Option<String>,
    /// Its parameter - its name, and the kind of value it takes (`name`, `number`) - which makes what reaches it a
    /// family: each member addressed by a value of it (`picture/night`).
    pub parameter: Option<(String, String)>,
    /// Which of several operations taking a source is offered first: the highest.
    pub priority: i32,
    pub run: Arc<RunOp>,
}

/// A family's members: each by the value of its parameter, with a label.
pub type Members = dyn Fn(&Asset) -> Vec<(String, String)> + Send + Sync;

/// An output a kind declares on every node of it: made by an operation taking the node's stack's result.
#[derive(Clone)]
pub struct OutputDef {
    pub name: String,
    /// The operation, qualified: a family where it has a parameter.
    pub op: String,
    pub retain: Retain,
    /// Its members, for a family.
    pub members: Option<Arc<Members>>,
}

#[cfg(test)]
mod tests {
    use super::{File, Type, Typed, Value};

    fn ty(text: &str) -> Type {
        Type::parse(text, "x.ext").unwrap()
    }

    fn fits(port: &str, given: &Type) -> bool {
        ty(port).unify(given, &mut Vec::new())
    }

    #[test]
    fn types_as_written() {
        assert_eq!(ty("indexed"), Type::Named("x.ext@indexed".into()));
        assert_eq!(ty("image/openraster"), Type::Named("image/openraster".into()));
        assert_eq!(ty("file"), Type::File(Box::new(Type::Any)));
        assert_eq!(ty("bs_picture | y.ext@indexed").to_string(), "x.ext@bs_picture | y.ext@indexed");
        assert_eq!(ty("list<file<T>>").to_string(), "list<file<T>>");
        assert!(Type::parse("a b", "x.ext").is_err());
    }

    #[test]
    fn where_a_value_lives_is_no_part_of_its_type() {
        // Recognized as several things, as one file: it fits a port naming any, and one asking for a file.
        let source = Typed { also: vec!["x.ext@zip".into(), "image/openraster".into()], ..Typed::new("x.ext@ora", vec![File::new("s.ora", vec![])]) };
        let given = Value::Typed(source).ty();
        assert!(fits("image/openraster", &given) && fits("zip", &given) && fits("file<ora>", &given) && fits("file", &given));
        assert!(!fits("png", &given));
        // Two files: still of its type; that it is no one file is found when it is given.
        let indexed = Value::Typed(Typed::new("x.ext@indexed", vec![File::new("a", vec![]), File::new("b", vec![])])).ty();
        assert!(fits("bs_picture | indexed", &indexed) && fits("file<indexed>", &indexed));
        // What the host holds in memory is no file.
        assert!(fits("any", &Type::Table) && !fits("file", &Type::Table) && !fits("file<any>", &Type::Image));
    }

    #[test]
    fn a_variable_is_bound_by_what_is_given_first() {
        let mut bound = Vec::new();
        assert!(ty("T").unify(&Type::Image, &mut bound));
        assert!(ty("T").unify(&Type::Image, &mut bound));
        assert!(!ty("T").unify(&Type::Table, &mut bound));
        assert_eq!(ty("list<T>").bind(&bound), Type::List(Box::new(Type::Image)));
    }
}
