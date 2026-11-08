//! Types as an extension's code names them: markers - zero-sized types implementing `ValueType`, which says the type
//! the host checks and how a value of it is stored: its Rust value, decoded from what the host moves and encoded back.
//! A type an extension registers is `Registered` too: its name, its label and what recognizes it in a stored file; one
//! whose value is best kept as its files - read when asked, by reference - is `Files`, and needs no more.
//!
//! The host's own types have markers here (`Image`, `Table`, `Pcm`, `Bytes`), as do its constructors (`File<T>`,
//! `List<T>`, `OneOf<(A, B)>`) and `Any`, which a generic operation takes - relating its ports by constraints
//! (`OpBuilder::same`) and given its values as `Data`. Another extension's types are its crate's markers; a media type,
//! which no extension owns, a marker of its own (`media_type!`).

use std::marker::PhantomData;

use crate::data::{Call, Pcm as PcmValue, Type, Typed, Value as Data};
use crate::{Image as Pixels, Marks as MarksValue, Recognized, Result, Signature, Table as TableValue, anyhow, bail};

/// A type as an extension's code names it: the type the host checks, and how a value of it is stored.
pub trait ValueType: 'static {
    /// A value of it in Rust: what an operation is given of it, and what one makes.
    type Value;
    /// The type, as the host checks it.
    #[must_use]
    fn ty() -> Type;
    /// A value the host gives, as this type's.
    ///
    /// # Errors
    /// It is of another type, or its encoding cannot be read.
    fn decode(data: Data, call: &Call<'_>) -> Result<Self::Value>;
    /// A value made, as the host moves and keeps it.
    fn encode(value: Self::Value) -> Data;
}

/// A sniff: a file's name, size and first bytes to whether it is of the type.
pub type SniffFn = fn(&str, u64, &[u8]) -> Option<Recognized>;

/// A type an extension registers: its name, its label, and what recognizes it in a stored file (none: it is only
/// ever made by an operation).
pub trait Registered: ValueType {
    /// Its name, qualified: `pe2.images@indexed`.
    const NAME: &'static str;
    const LABEL: &'static str;
    /// A media type it answers to (`image/png`).
    const MEDIA_TYPE: Option<&'static str> = None;
    /// Its sniff, where signatures leave it open.
    const SNIFF: Option<SniffFn> = None;
    /// What recognizes it without asking the extension: bytes at an offset, a file name, text.
    #[must_use]
    fn signatures() -> Vec<Signature> {
        Vec::new()
    }
}

/// What a package compiles from, gathered by `pe2.packages` for a target: each file by its place. `self/<path>` for the package's own,
/// `dep/<name>/<path>` for a dependency's public file (`#include <name/path>`), what the build makes for it (`pe2_options.h`) at the top,
/// `units` listing what to compile, and `target/<target>/<package>/<key>/<name>` for what a package names under `target.<target>`.
pub struct SourceTree;

impl Registered for SourceTree {
    const NAME: &'static str = "pe2.packages@tree";
    const LABEL: &'static str = "Source tree";
}

impl Files for SourceTree {}

/// What a value is in the language a package is written in: the text of a tokenizer (`crate::tokens`).
pub struct Tokens;

impl Tokens {
    /// The one file it is.
    pub const FILE: &'static str = "tokens";
}

impl Registered for Tokens {
    const NAME: &'static str = "pe2.packages@tokens";
    const LABEL: &'static str = "Tokens";
}

impl Files for Tokens {}

/// A registered type whose value is kept as its files - its encoding - read when asked, by reference.
pub trait Files: Registered<Value = Typed> {}

impl<T: Files> ValueType for T {
    type Value = Typed;

    fn ty() -> Type {
        Type::Named(T::NAME.to_string())
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Typed> {
        typed(data, T::NAME)
    }

    fn encode(value: Typed) -> Data {
        Data::Typed(value)
    }
}

/// A value as files of a type: given as it, or answering to it.
fn typed(data: Data, name: &str) -> Result<Typed> {
    match data {
        Data::Typed(t) if t.ty == name || t.also.iter().any(|a| a == name) => Ok(t),
        other => bail!("given {}, not {name}", other.ty()),
    }
}

/// Declares a marker for a media type: what a file answers to whoever recognizes it (`image/openraster`).
#[macro_export]
macro_rules! media_type {
    ($(#[$meta:meta])* $vis:vis $marker:ident = $media:literal) => {
        $(#[$meta])*
        $vis struct $marker;

        impl $crate::ty::ValueType for $marker {
            type Value = $crate::Typed;

            fn ty() -> $crate::Type {
                $crate::Type::Named($media.to_string())
            }

            fn decode(data: $crate::Data, _: &$crate::Call<'_>) -> $crate::Result<$crate::Typed> {
                match data {
                    $crate::Data::Typed(t) if t.ty == $media || t.also.iter().any(|a| a == $media) => Ok(t),
                    other => $crate::bail!("given {}, not {}", other.ty(), $media),
                }
            }

            fn encode(value: $crate::Typed) -> $crate::Data {
                $crate::Data::Typed(value)
            }
        }
    };
}

/// What an operation's parameter takes: its values, written in addresses (`picture/night`), read back.
pub trait Parameter: Sized + 'static {
    /// The type of value it is, as described: `name`, `number`.
    const KIND: &'static str;
    /// A value as an address writes it.
    fn to_text(&self) -> String;
    /// A value as an address writes it, read.
    ///
    /// # Errors
    /// It is no value of it.
    fn from_text(text: &str) -> Result<Self>;
}

impl Parameter for String {
    const KIND: &str = "name";

    fn to_text(&self) -> String {
        self.clone()
    }

    fn from_text(text: &str) -> Result<String> {
        Ok(text.to_string())
    }
}

impl Parameter for u32 {
    const KIND: &str = "number";

    fn to_text(&self) -> String {
        self.to_string()
    }

    fn from_text(text: &str) -> Result<u32> {
        text.parse().map_err(|_| anyhow!("`{text}` is no number"))
    }
}

/// Whatever it is given, as `Data`: what a generic operation takes, its ports related by constraints.
pub struct Any;

impl ValueType for Any {
    type Value = Data;

    fn ty() -> Type {
        Type::Any
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Data> {
        Ok(data)
    }

    fn encode(value: Data) -> Data {
        value
    }
}

/// A picture, as RGBA.
pub struct Image;

impl ValueType for Image {
    type Value = Pixels;

    fn ty() -> Type {
        Type::Image
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Pixels> {
        match data {
            Data::Image(image) => Ok(image),
            other => bail!("given {}, not an image", other.ty()),
        }
    }

    fn encode(value: Pixels) -> Data {
        Data::Image(value)
    }
}

/// A table.
pub struct Table;

impl ValueType for Table {
    type Value = TableValue;

    fn ty() -> Type {
        Type::Table
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<TableValue> {
        match data {
            Data::Table(table) => Ok(table),
            other => bail!("given {}, not a table", other.ty()),
        }
    }

    fn encode(value: TableValue) -> Data {
        Data::Table(value)
    }
}

/// Audio, as PCM.
pub struct Pcm;

impl ValueType for Pcm {
    type Value = PcmValue;

    fn ty() -> Type {
        Type::Pcm
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<PcmValue> {
        match data {
            Data::Pcm(pcm) => Ok(pcm),
            other => bail!("given {}, not audio", other.ty()),
        }
    }

    fn encode(value: PcmValue) -> Data {
        Data::Pcm(value)
    }
}

/// A stream of a type's items, given to an operation to read in turn.
pub struct Stream<T>(PhantomData<T>);

impl<T: ValueType> ValueType for Stream<T> {
    type Value = crate::data::StreamInput;

    fn ty() -> Type {
        Type::Stream(Box::new(T::ty()))
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<crate::data::StreamInput> {
        match data {
            Data::Stream(stream) => Ok(stream),
            other => bail!("given {}, not a stream", other.ty()),
        }
    }

    fn encode(value: crate::data::StreamInput) -> Data {
        Data::Stream(value)
    }
}

/// An item of a stream of moving pictures, whoever makes or reads it: a frame's picture and the sound up to it, each
/// where there is one.
pub struct Playback;

impl ValueType for Playback {
    type Value = Data;

    fn ty() -> Type {
        Type::List(Box::new(Type::Union(vec![Type::Video, Type::Audio])))
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Data> {
        Ok(data)
    }

    fn encode(value: Data) -> Data {
        value
    }
}

/// An item of a stream of sound alone, whoever makes or reads it: a stretch of its samples. No picture comes with it, so
/// what takes moving pictures (a video file) does not take it.
pub struct Sound;

impl ValueType for Sound {
    type Value = Data;

    fn ty() -> Type {
        Type::Audio
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Data> {
        Ok(data)
    }

    fn encode(value: Data) -> Data {
        value
    }
}

/// What is marked on a node's pictures.
pub struct Marks;

impl ValueType for Marks {
    type Value = MarksValue;

    fn ty() -> Type {
        Type::Marks
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<MarksValue> {
        match data {
            Data::Marks(marks) => Ok(marks),
            other => bail!("given {}, not marks", other.ty()),
        }
    }

    fn encode(value: MarksValue) -> Data {
        Data::Marks(value)
    }
}

/// Data nothing recognizes, as its files.
pub struct Bytes;

impl ValueType for Bytes {
    type Value = Typed;

    fn ty() -> Type {
        Type::Bytes
    }

    fn decode(data: Data, _: &Call<'_>) -> Result<Typed> {
        match data {
            Data::Typed(t) => Ok(t),
            other => bail!("given {}, not files", other.ty()),
        }
    }

    fn encode(value: Typed) -> Data {
        Data::Typed(value)
    }
}

/// A value of a type as one file: a representation, which the host makes it take.
pub struct File<T>(PhantomData<T>);

impl<T: ValueType> ValueType for File<T> {
    type Value = T::Value;

    fn ty() -> Type {
        Type::File(Box::new(T::ty()))
    }

    fn decode(data: Data, call: &Call<'_>) -> Result<T::Value> {
        if let Data::Typed(t) = &data {
            t.file()?;
        }
        T::decode(data, call)
    }

    fn encode(value: T::Value) -> Data {
        T::encode(value)
    }
}

/// A list of values of a type.
pub struct List<T>(PhantomData<T>);

impl<T: ValueType> ValueType for List<T> {
    type Value = Vec<T::Value>;

    fn ty() -> Type {
        Type::List(Box::new(T::ty()))
    }

    fn decode(data: Data, call: &Call<'_>) -> Result<Vec<T::Value>> {
        match data {
            Data::List(items) => items.into_iter().map(|item| T::decode(item, call)).collect(),
            other => bail!("given {}, not a list", other.ty()),
        }
    }

    fn encode(value: Vec<T::Value>) -> Data {
        Data::List(value.into_iter().map(T::encode).collect())
    }
}

/// A value of one of several types (`OneOf<(A, B)>`), given as which it is.
pub struct OneOf<T>(PhantomData<T>);

/// A value of one of two types.
pub enum Either<A, B> {
    First(A),
    Second(B),
}

/// A value of one of three types.
pub enum Either3<A, B, C> {
    First(A),
    Second(B),
    Third(C),
}

/// Whether a value is of a type.
fn is<T: ValueType>(data: &Data) -> bool {
    T::ty().unify(&data.ty(), &mut Vec::new())
}

impl<A: ValueType, B: ValueType> ValueType for OneOf<(A, B)> {
    type Value = Either<A::Value, B::Value>;

    fn ty() -> Type {
        Type::Union(vec![A::ty(), B::ty()])
    }

    fn decode(data: Data, call: &Call<'_>) -> Result<Self::Value> {
        if is::<A>(&data) {
            A::decode(data, call).map(Either::First)
        } else if is::<B>(&data) {
            B::decode(data, call).map(Either::Second)
        } else {
            Err(anyhow!("given {}, not {}", data.ty(), Self::ty()))
        }
    }

    fn encode(value: Self::Value) -> Data {
        match value {
            Either::First(a) => A::encode(a),
            Either::Second(b) => B::encode(b),
        }
    }
}

impl<A: ValueType, B: ValueType, C: ValueType> ValueType for OneOf<(A, B, C)> {
    type Value = Either3<A::Value, B::Value, C::Value>;

    fn ty() -> Type {
        Type::Union(vec![A::ty(), B::ty(), C::ty()])
    }

    fn decode(data: Data, call: &Call<'_>) -> Result<Self::Value> {
        if is::<A>(&data) {
            A::decode(data, call).map(Either3::First)
        } else if is::<B>(&data) {
            B::decode(data, call).map(Either3::Second)
        } else if is::<C>(&data) {
            C::decode(data, call).map(Either3::Third)
        } else {
            Err(anyhow!("given {}, not {}", data.ty(), Self::ty()))
        }
    }

    fn encode(value: Self::Value) -> Data {
        match value {
            Either3::First(a) => A::encode(a),
            Either3::Second(b) => B::encode(b),
            Either3::Third(c) => C::encode(c),
        }
    }
}

impl<T> Either<T, T> {
    /// The value, whichever of two types of one storage it is.
    pub fn into_inner(self) -> T {
        match self {
            Either::First(v) | Either::Second(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Any, Files, Image, List, OneOf, Registered, ValueType};
    use crate::{Registry, Type};

    struct Texture;

    impl Registered for Texture {
        const NAME: &str = "x.ext@texture";
        const LABEL: &str = "A texture";
    }

    impl Files for Texture {}

    #[test]
    fn markers_are_types() {
        assert_eq!(<OneOf<(Texture, Image)>>::ty().to_string(), "x.ext@texture | image");
        assert_eq!(<List<super::File<Texture>>>::ty().to_string(), "list<file<x.ext@texture>>");
    }

    #[test]
    fn constraints_relate_ports_taking_anything() {
        let mut r = Registry::new("x.ext");
        let _ = r.value_type::<Texture>();
        let _ = r
            .op("select")
            .input::<Any>("a")
            .input::<Any>("b")
            .output::<Any>()
            .same("a", "b")
            .output_same_as("a")
            .run(|call| call.take::<Any>(0));
        let _ =
            r.op("join").inputs::<Any>("parts").output::<Any>().output_list_of("parts").run(|call| call.take::<Any>(0));
        let _ = r.op("wrong").input::<Texture>("t").output::<Any>().output_same_as("t").run(|call| call.take::<Any>(0));
        let parts = r.into_parts();
        let select = &parts.ops[0];
        let var = Type::Var("A".into());
        assert_eq!((&select.inputs[0].ty, &select.inputs[1].ty, &select.output), (&var, &var, &var));
        assert_eq!(parts.ops[1].output, Type::List(Box::new(var)));
        // A typed port is no variable: a constraint on it is the extension's mistake.
        assert_eq!(parts.problems, ["x.ext@wrong: a constraint relates a port of a type"]);
    }
}
