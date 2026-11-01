//! The work graph as an extension sees it: values and their types, the operations it registers, and the outputs its
//! types declare.
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
    /// A stream of values of a type: a header, then its items made as they are read (`Generator`). Never a value: an
    /// output that is one is opened, not made.
    Stream(Box<Type>),
    /// A frame, in a buffer the host holds (`media::VideoFrame`).
    Video,
    /// A stretch of sound, in a buffer the host holds (`media::AudioData`).
    Audio,
    /// What is marked on a node's pictures (`Marks`).
    Marks,
}

impl Type {
    /// A type as it is written: `any`, `image`, `table`, `pcm`, `bytes`, a variable (one capital letter, `T`),
    /// `list<T>`, `file<T>` (`file` alone: `file<any>`), `stream<T>`, a union (`a | b`), or a type's name - bare, qualified by `id`
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
            "video" => Type::Video,
            "audio" => Type::Audio,
            "marks" => Type::Marks,
            "bytes" => Type::Bytes,
            "file" => Type::File(Box::new(Type::Any)),
            _ if inner("file<").is_some() => Type::File(Box::new(Type::parse(inner("file<").unwrap_or_default(), id)?)),
            _ if inner("list<").is_some() => Type::List(Box::new(Type::parse(inner("list<").unwrap_or_default(), id)?)),
            _ if inner("stream<").is_some() => Type::Stream(Box::new(Type::parse(inner("stream<").unwrap_or_default(), id)?)),
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
            (Type::File(_), Type::Image | Type::Table | Type::Pcm | Type::Video | Type::Audio | Type::Marks | Type::List(_)) => false,
            (Type::File(port), given) => port.unify(given, bound),
            (port, Type::File(given)) => port.unify(given.as_ref(), bound),
            (Type::List(a), Type::List(b)) | (Type::Stream(a), Type::Stream(b)) => a.unify(b, bound),
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
            Type::Stream(item) => Type::Stream(Box::new(item.bind(bound))),
            Type::Union(any) => Type::Union(any.iter().map(|t| t.bind(bound)).collect()),
            _ => self.clone(),
        }
    }

    /// Whether a port of it asks for a file.
    #[must_use]
    pub fn wants_file(&self) -> bool {
        matches!(self, Type::File(_))
    }

    /// Whether it is a stream: what an output of it is opened as.
    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self, Type::Stream(_))
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
            Type::Video => f.write_str("video"),
            Type::Audio => f.write_str("audio"),
            Type::Marks => f.write_str("marks"),
            Type::Bytes => f.write_str("bytes"),
            Type::List(item) => write!(f, "list<{item}>"),
            Type::Stream(item) => write!(f, "stream<{item}>"),
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
    Video(crate::media::VideoFrame),
    Audio(crate::media::AudioData),
    Marks(crate::Marks),
    Stream(StreamInput),
}

/// What reads a stream for an operation it is given to, in the host: its items in turn.
pub trait StreamSource: Send + Sync {
    fn header(&self) -> Table;
    /// The next item, waiting for it; none at the stream's end.
    ///
    /// # Errors
    /// It could not be made.
    fn next(&self) -> Result<Option<Value>>;
    /// Go on from an item.
    fn seek(&self, position: u64);
}

/// A stream given to an operation: read in turn. In the host, its reader; in a component, a handle to it.
#[derive(Clone)]
pub struct StreamInput {
    #[cfg(not(target_family = "wasm"))]
    source: Arc<dyn StreamSource>,
    #[cfg(target_family = "wasm")]
    handle: std::rc::Rc<crate::guest::bindings::pe2::extension::types::StreamReader>,
}

impl StreamInput {
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn of(source: Arc<dyn StreamSource>) -> StreamInput {
        StreamInput { source }
    }

    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn source(&self) -> &Arc<dyn StreamSource> {
        &self.source
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn from_host(handle: crate::guest::bindings::pe2::extension::types::StreamReader) -> StreamInput {
        StreamInput { handle: std::rc::Rc::new(handle) }
    }

    /// What the stream is (`Generator::header`).
    #[must_use]
    pub fn header(&self) -> Table {
        #[cfg(not(target_family = "wasm"))]
        return self.source.header();
        #[cfg(target_family = "wasm")]
        toml::from_str(&self.handle.header()).unwrap_or_default()
    }

    /// The next item, waiting for it; none at the stream's end.
    ///
    /// # Errors
    /// It could not be made.
    pub fn next(&self) -> Result<Option<Value>> {
        #[cfg(not(target_family = "wasm"))]
        return self.source.next();
        #[cfg(target_family = "wasm")]
        self.handle.next().map(|item| item.map(crate::guest::from_wit)).map_err(|e| anyhow!(e))
    }

    /// Go on from an item.
    pub fn seek(&self, position: u64) {
        #[cfg(not(target_family = "wasm"))]
        self.source.seek(position);
        #[cfg(target_family = "wasm")]
        self.handle.seek(position);
    }
}

impl fmt::Debug for StreamInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StreamInput")
    }
}

impl PartialEq for StreamInput {
    fn eq(&self, other: &StreamInput) -> bool {
        #[cfg(not(target_family = "wasm"))]
        return Arc::ptr_eq(&self.source, &other.source);
        #[cfg(target_family = "wasm")]
        std::rc::Rc::ptr_eq(&self.handle, &other.handle)
    }
}

impl Value {
    /// What comes with it: for a value of an extension's type, what was set on the layers it came through and what they
    /// derived; for a table, what it holds.
    #[must_use]
    pub fn settings(&self) -> Option<&Table> {
        match self {
            Value::Typed(typed) => Some(&typed.settings),
            Value::Table(table) => Some(table),
            _ => None,
        }
    }

    #[must_use]
    pub fn ty(&self) -> Type {
        match self {
            Value::Image(_) => Type::Image,
            Value::Table(_) => Type::Table,
            Value::Pcm(_) => Type::Pcm,
            Value::Video(_) => Type::Video,
            Value::Audio(_) => Type::Audio,
            Value::Marks(_) => Type::Marks,
            Value::Stream(_) => Type::Stream(Box::new(Type::Any)),
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
    /// For the run making it: an epoch, or an attach, gives it to whoever asks while what it read holds, and forgets it
    /// when it ends. Asked for outside a run, it is made for whoever asks.
    Transient,
}

/// What an operation is lent of its node and the host while it runs.
pub trait CallHost {
    /// Its node's path.
    fn path(&self) -> &str;
    /// Its node's type, qualified.
    fn node_type(&self) -> Option<&str>;
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
    /// A file the operation makes, named `name`, written a piece at a time: held where the host holds what is large
    /// (its store), or - by a host that holds nothing - in memory.
    ///
    /// # Errors
    /// The host cannot make one.
    fn writer(&self, name: &str) -> Result<Box<dyn FileWriter>> {
        Ok(Box::new(InMemory { name: name.to_string(), bytes: Vec::new() }))
    }
    /// A path for a program to write to, for an unsandboxed extension's operation.
    ///
    /// # Errors
    /// The host has none to give.
    fn temp(&self) -> Result<String> {
        bail!("{}: no temporary files here", self.path())
    }
    /// A file an unsandboxed extension's program made kept as a content, moved rather than read.
    ///
    /// # Errors
    /// It cannot be kept.
    fn keep(&self, path: &str, name: &str) -> Result<File> {
        let _ = (path, name);
        bail!("{}: nothing is kept here", self.path())
    }
    /// The plain files under a folder of the project, each by its path, as the stack resolves it; recorded as read, so what is made
    /// is made again when the list changes.
    ///
    /// # Errors
    /// The project cannot be reached from here.
    fn entries(&self, folder: &str) -> Result<Vec<String>> {
        bail!("{}: {folder}: no project to list here", self.path())
    }
    /// A plain file of the project by its path, as the stack resolves it; recorded as read.
    ///
    /// # Errors
    /// The project cannot be reached from here, or holds no such file.
    fn read_entry(&self, path: &str) -> Result<Vec<u8>> {
        bail!("{}: {path}: no project to read here", self.path())
    }
    /// The assets of a type, each by its path with its declaration as written; recorded as read.
    ///
    /// # Errors
    /// The project cannot be reached from here.
    fn assets_of(&self, node_type: &str) -> Result<Vec<(String, String)>> {
        bail!("{}: {node_type}: no project to look in here", self.path())
    }
    /// Where a file or folder of the project is on disk, followed through links; none where it has no such place.
    fn place(&self, path: &str) -> Option<String> {
        let _ = path;
        None
    }
    /// An output of another asset, by its path or a reference to it; recorded as read.
    ///
    /// # Errors
    /// The project cannot be reached from here, or it could not be made.
    fn output_of(&self, path: &str, address: &str) -> Result<Value> {
        bail!("{path}: {address}: no project to look in here")
    }
    /// The node a reference names.
    fn node_named(&self, reference: &str) -> Option<NodeNamed> {
        let _ = reference;
        None
    }
    /// The config being built; recorded as read, so what reads it is made again for another.
    fn build_config(&self) -> Option<BuildConfig> {
        None
    }
    /// Where a content is on disk, for an unsandboxed extension's operation to point a program at.
    fn path_of(&self, content: &Content) -> Option<String> {
        let _ = content;
        None
    }
}

/// A node as a reference names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeNamed {
    pub path: String,
    pub uid: Option<String>,
    /// What it is, each type qualified: what its stack makes, first what it comes to.
    pub types: Vec<String>,
}

/// A file being made: appended to, then finished into its file - its content inline, or a blob the host keeps.
pub trait FileWriter: Send {
    /// # Errors
    /// What is appended cannot be kept.
    fn append(&mut self, bytes: &[u8]) -> Result<()>;
    /// # Errors
    /// What was appended cannot be kept.
    fn finish(self: Box<Self>) -> Result<File>;
}

/// A file made in memory, its content inline.
struct InMemory {
    name: String,
    bytes: Vec<u8>,
}

impl FileWriter for InMemory {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<File> {
        Ok(File::new(&self.name, self.bytes))
    }
}

/// A file an operation makes (`Call::writer`), as `std::io::Write`: what is written gathered into pieces of `PIECE`
/// bytes, each appended to the host's writer, then finished into its file (`finish`). What is large is never held
/// whole - by a component, never in its memory.
pub struct Writer {
    inner: Box<dyn FileWriter>,
    piece: Vec<u8>,
}

impl Writer {
    /// How much is gathered before it is appended.
    pub const PIECE: usize = 256 << 10;

    /// The file made: its content inline where it is small, else kept by the host.
    ///
    /// # Errors
    /// What was written cannot be kept.
    pub fn finish(mut self) -> Result<File> {
        if !self.piece.is_empty() {
            self.inner.append(&self.piece)?;
        }
        self.inner.finish()
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.piece.extend_from_slice(bytes);
        if self.piece.len() >= Writer::PIECE {
            self.inner.append(&self.piece).map_err(std::io::Error::other)?;
            self.piece.clear();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An operation running: the values it is given by port, its settings, the value of its parameter as its output's
/// address gives it (`night` of `picture/night`; read by `parameter`), and its host.
pub struct Call<'a> {
    pub inputs: Vec<Vec<Value>>,
    pub settings: &'a Table,
    /// What came with the value given to its first port.
    pub carried: Table,
    /// Its outputs after the first, as it makes them (`make`).
    pub made: std::cell::RefCell<std::collections::BTreeMap<String, Value>>,
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

    /// Make one of its outputs after the first.
    pub fn make(&self, output: &str, value: Value) {
        self.made.borrow_mut().insert(output.to_string(), value);
    }

    /// What comes with these inputs: what came with the value given to the first port.
    #[must_use]
    pub fn carried_by(inputs: &[Vec<Value>]) -> Table {
        inputs.first().and_then(|port| port.first()).and_then(Value::settings).cloned().unwrap_or_default()
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

    /// A file it makes, named `name`, written through `std::io::Write` and finished into its `File`: what is large is
    /// kept by the host as it is written (`CallHost::writer`) rather than held.
    ///
    /// # Errors
    /// The host cannot make one.
    pub fn writer(&self, name: &str) -> Result<Writer> {
        Ok(Writer { inner: self.host.writer(name)?, piece: Vec::new() })
    }

    /// A path for a program of this extension to write to: `keep` it, or remove it.
    ///
    /// # Errors
    /// The host has none to give.
    pub fn temp(&self) -> Result<String> {
        self.host.temp()
    }

    /// A file a program of this extension made kept as a content, named `name`.
    ///
    /// # Errors
    /// It cannot be kept.
    pub fn keep(&self, path: &str, name: &str) -> Result<File> {
        self.host.keep(path, name)
    }

    /// The plain files under a folder of the project; made again when the list changes.
    ///
    /// # Errors
    /// The project cannot be reached from here.
    pub fn entries(&self, folder: &str) -> Result<Vec<String>> {
        self.host.entries(folder)
    }

    /// A plain file of the project; made again when it changes.
    ///
    /// # Errors
    /// The project cannot be reached from here, or holds no such file.
    pub fn read_entry(&self, path: &str) -> Result<Vec<u8>> {
        self.host.read_entry(path)
    }

    /// The assets of a type, each by its path with its declaration as written; made again when any changes, or one comes or goes.
    ///
    /// # Errors
    /// The project cannot be reached from here.
    pub fn assets_of(&self, node_type: &str) -> Result<Vec<(String, String)>> {
        self.host.assets_of(node_type)
    }

    /// Where a file or folder of the project is on disk, followed through links.
    #[must_use]
    pub fn place(&self, path: &str) -> Option<String> {
        self.host.place(path)
    }

    /// An output of another asset, by its path or a reference to it; what reads it is made again when it changes.
    ///
    /// # Errors
    /// The project cannot be reached from here, or it could not be made.
    pub fn output_of(&self, path: &str, address: &str) -> Result<Value> {
        self.host.output_of(path, address)
    }

    /// The node a reference names; none where nothing is so named.
    #[must_use]
    pub fn node_named(&self, reference: &str) -> Option<NodeNamed> {
        self.host.node_named(reference)
    }

    /// An output of another asset that is a table.
    ///
    /// # Errors
    /// As `output_of`, or it is no table.
    pub fn table_of(&self, path: &str, address: &str) -> Result<Table> {
        match self.output_of(path, address)? {
            Value::Table(table) => Ok(table),
            other => bail!("{path}: {address} is {}, no table", other.ty()),
        }
    }

    /// The config being built; made again for another.
    #[must_use]
    pub fn config(&self) -> Option<BuildConfig> {
        self.host.build_config()
    }

    /// Where a file is on disk, for this extension to point a program at; none where it is held in memory.
    #[must_use]
    pub fn path_of(&self, file: &File) -> Option<String> {
        self.host.path_of(&file.content)
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

    /// Lend a value of an extension's type as an asset: its files, and what came with it, what this layer sets over that.
    #[must_use]
    pub fn lend<'b>(&'b self, typed: &'b Typed) -> Lent<'b> {
        Lent { host: self.host, files: &typed.files, fields: merged(&typed.settings, self.settings) }
    }

    /// Lend what came with a value, what this layer sets over that, as an asset of no files.
    #[must_use]
    pub fn lend_settings(&self, derived: &Table) -> Lent<'_> {
        Lent { host: self.host, files: &[], fields: merged(derived, self.settings) }
    }

    /// Lend files as an asset, with what the operation is set to.
    #[must_use]
    pub fn lend_files<'b>(&'b self, files: &'b [File]) -> Lent<'b> {
        Lent { host: self.host, files, fields: self.given() }
    }

    /// What it is set to: what is set on its own layer, over what came with the value given to its first port. A value
    /// comes with what was set on the layers it came through and what they derived, by name, so an operation reads there
    /// what an earlier one holds, and what it reads is what it is given.
    #[must_use]
    pub fn given(&self) -> Table {
        merged(&self.carried, self.settings)
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
    /// As an asset of its node's type.
    #[must_use]
    pub fn asset(&'a self) -> Asset<'a> {
        Asset::new(self, self.host.node_type())
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

/// What a stream operation does when it is opened: given its inputs, settings and parameter, it starts its generator.
pub type StartStream = dyn Fn(&mut Call<'_>) -> Result<Box<dyn Generator>> + Send + Sync;

/// What makes a stream's items, a batch at a time, as its readers come near the end of those made: started by a
/// stream operation from what it is given (keeping what it reads by - its inputs' files, by content), then asked for
/// more, or to go on from elsewhere. Each call is given a `Call` of no inputs - the node's settings, its contents by
/// hash - through which it reads what it kept.
pub trait Generator: Send {
    /// What the stream is: a table its readers understand (`rate`, items a second; `count`, where it is known; what
    /// its items hold).
    fn header(&self) -> Table;
    /// Up to `count` items after the last: fewer, or none, where the stream ends.
    ///
    /// # Errors
    /// They cannot be made.
    fn next(&mut self, call: &Call<'_>, count: usize) -> Result<Vec<Value>>;
    /// Go on from the item at `position`: whether it can (else it goes on as it was, and is read up to there).
    ///
    /// # Errors
    /// It could not get there.
    fn seek(&mut self, call: &Call<'_>, position: u64) -> Result<bool> {
        let _ = (call, position);
        Ok(false)
    }
}

/// The config being built: `[config.<name>]`, with the folder its results go to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildConfig {
    pub name: String,
    pub release: Option<String>,
    pub target: Option<String>,
    pub folder: String,
    /// What every config of the project says, as `key=value`: what a source may test for.
    pub known: Vec<String>,
}

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
    /// Its parameter - its name, and the type of value it takes (`name`, `number`) - which makes what reaches it a
    /// family: each member addressed by a value of it (`picture/night`).
    pub parameter: Option<(String, String)>,
    /// Which of several operations taking a source is offered first: the highest.
    pub priority: i32,
    /// What it makes of what it takes for whoever asks by name.
    pub provides: Option<Provides>,
    /// The members of what it makes, for a family.
    pub members: Option<Arc<Members>>,
    /// Its outputs after the first, by name.
    pub also: Vec<(String, Type)>,
    pub run: Arc<RunOp>,
    /// What it will read, said before it runs (`OpBuilder::prepare`). What says nothing reads nothing of other nodes.
    pub prepare: Option<Arc<PrepareOp>>,
    /// For a stream operation (its output a `stream<T>`): how it is opened.
    pub start: Option<Arc<StartStream>>,
}

/// What says what an operation will read.
pub type PrepareOp = dyn Fn(&mut Call<'_>) -> Result<Prepared> + Send + Sync;

/// An output of another node an operation will read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    /// The node, by its path or a reference to it.
    pub node: String,
    pub address: String,
}

impl Need {
    #[must_use]
    pub fn new(node: &str, address: &str) -> Need {
        Need { node: node.to_string(), address: address.to_string() }
    }
}

/// Where a micro op's value comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicroInput {
    /// An earlier micro op's value, by its place; otherwise what the operation was given at `port`.
    pub micro: Option<u32>,
    pub port: u32,
    /// Of a value holding files, only these: each a whole name, or a folder ending in `/`. None: all of it.
    pub files: Vec<String>,
}

impl MicroInput {
    /// What the operation was given at a port.
    #[must_use]
    pub fn port(port: u32) -> MicroInput {
        MicroInput { micro: None, port, files: Vec::new() }
    }

    /// An earlier micro op's value.
    #[must_use]
    pub fn micro(at: usize) -> MicroInput {
        MicroInput { micro: u32::try_from(at).ok(), port: 0, files: Vec::new() }
    }

    /// Only these of its files.
    #[must_use]
    pub fn only(mut self, files: &[&str]) -> MicroInput {
        self.files = files.iter().map(ToString::to_string).collect();
        self
    }
}

/// A piece of an operation's work: another operation of its extension.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Micro {
    pub op: String,
    /// What tells it from the others: the same between runs.
    pub label: String,
    pub settings: Table,
    pub ports: Vec<Vec<MicroInput>>,
    /// Kept, and made once for what it takes and reads.
    pub keep: bool,
}

/// What an operation says before it runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    /// What it will read: made first, with no instance of it held meanwhile.
    pub needs: Vec<Need>,
    /// Where it splits, what does its work in its place, each taking from those before it. The last one's value is its.
    pub micros: Vec<Micro>,
}

/// A family's members: each by the value of its parameter, with a label.
pub type Members = dyn Fn(&Asset) -> Vec<(String, String)> + Send + Sync;

/// What an operation makes of a value of what it takes, for whoever asks a node making one by this name: a conversion, which
/// no stack holds (`tokens`, a node's `reflection`). What a node itself offers is in its stack, a template writing it there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provides {
    pub name: String,
    pub retain: Retain,
}

/// An output a node has, as what lists a node's outputs reads it.
#[derive(Clone)]
pub struct OutputDef {
    pub name: String,
    /// What makes it, qualified: a family where that has a parameter.
    pub op: String,
    pub ty: Type,
    pub retain: Retain,
    pub members: Option<Arc<Members>>,
    /// Shown by a viewer of what it is; it takes part in no build.
    pub preview: bool,
    /// One of it for each key of this table of a node's, under that key's name; its members are listed `<key>/<member>`.
    pub each: Option<String>,
    /// A layer of its stack, not a conversion.
    pub in_stack: bool,
    /// The conversion it is made from; none, a step of its stack.
    pub from: Option<String>,
    /// The step of its stack it is made from.
    pub step: Option<usize>,
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
