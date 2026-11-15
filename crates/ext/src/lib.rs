//! The extension SDK: what an extension of the editor is written against.
//!
//! An extension has an id (`pe2.images`), which namespaces everything it registers: its types are named
//! `<id>@<name>` (`pe2.images@image`), so no two extensions' names collide and every name says who owns it. It
//! registers procedurally, from one entry point (`extension!`): a type and what it attaches to it - its fields as the
//! inspector shows them, its outputs, its views - and the types and
//! operations its nodes' stacks are made of (`data`).
//!
//! An extension is built as a WebAssembly component (`wasm32-wasip3`): `extension!` then exports it through the
//! contract in `wit/extension.wit` (`guest`), and the host (`project::ext`) loads it with wasmtime. Built for the host's
//! own target, the same crate links into the editor instead and the host calls what it registers directly - the
//! built-ins' fallback while developing. What crosses is what WIT can say: assets lent for a call, images, a `Ui`
//! handle a view draws through, never the host's own types.

mod asset;
mod buffer;
pub mod cbor;
pub mod data;
pub mod events;
mod extract;
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub mod guest;
mod image;
mod import;
pub mod media;
mod place;
pub mod process;
mod project;
mod registry;
pub mod stepped;
pub mod test;
pub mod tokens;
pub mod ty;
mod ui;

pub use anyhow::{Error, Result, anyhow, bail};
pub use asset::{Asset, Source};
pub use buffer::Buffer;
pub use data::{
    BuildConfig, Call, CallHost, Content, Export, File, FileWriter, Generator, INLINE, Lent, Members, Micro,
    MicroInput, Need, NodeNamed, NotSha1, OpDef, OutputDef, Pcm, Port, PrepareOp, Prepared, Provides, Retain, RunOp,
    Sha1, StartStream, StreamInput, StreamSource, Type, Typed, Use, Value as Data, Writer, merged, targeted,
};
pub use extract::{Declared, Discs, Given, Made};
pub use image::{Image, Mark, Marks, Rect, Texture};
pub use import::{Recognized, Signature, Sniff, TypeDef};
pub use pe2_ext_macros::{extension, steps};
pub use pe2_path::{Address, Asked, PathError, Query, Reference, RelPath, ResourcePath, Uid};
/// A path in the project's tree: what an asset is found by, beside its uid.
pub type TreePath = pe2_path::VfsPath<pe2_path::Tree>;
/// The assets of a type, each by its path with its declaration's text (`CallHost::assets_of`).
pub type AssetsOf = Box<[(std::sync::Arc<TreePath>, Box<str>)]>;
pub use place::{Arg, ArgPart, HostPath, Place, PlacedContent};
pub use project::{Project, Requested};
pub use registry::{
    ActOnContents, CommandDef, ContentAction, ContentDrag, ContentOutcome, ContentPlace, ContentRequest, ContentRow,
    ContentStyle, Contents, Extension, Field, Format, HookBuilder, HookDef, Icon, MakePanel, MakeView, Name, OpBuilder,
    PanelDef, PartField, PartSheet, Parts, Registry, Run, Scope, Setting, SheetOfPart, TabDef, TemplateDef, Tint,
    ToolDef, TypeBuilder, ViewDef, section,
};
pub use toml::{Table, Value};

/// The output every node has: what it is, in its native form.
pub const NATIVE: &Asked = Asked::constant("native");
/// The output every node has: its settings, as declared.
pub const SETTINGS: &Asked = Asked::constant("settings");

/// The node a reference names, as a host call asks for it: by its uid, or by its path, whatever holds it
/// (`{ slot = ... }`), without what of it it names (`uid://x` of `uid://x/cells/body?palette=default`). None where it
/// holds no reference.
///
/// # Errors
/// What it holds names no node: no uid, or no path.
pub fn target_of(reference: &Value) -> Result<Option<Reference>> {
    let reference = reference.get("slot").unwrap_or(reference);
    let reference = reference.get("slot").unwrap_or(reference);
    let named = match reference {
        Value::String(text) => match text.strip_prefix(UID_PREFIX) {
            Some(digits) => &text[..UID_PREFIX.len() + digits.find(['/', '?', '#']).unwrap_or(digits.len())],
            None => text.as_str(),
        },
        Value::Table(table) => match table.get("uid") {
            Some(Value::String(uid)) => uid.as_str(),
            Some(other) => bail!("its `uid` is {}, no text", other.type_str()),
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(Some(Reference::new(named)?))
}

/// The node a reference written with its bundle's path names (`{ uid, path = "res://<path>.node" }`), by that path: what
/// declarations a plan gives, which no index resolves, name nodes by. None where it says no path.
///
/// # Errors
/// What it says is no such path.
pub fn declared_path(reference: &Value) -> Result<Option<std::sync::Arc<TreePath>>> {
    let Some(written) = reference.get("path") else { return Ok(None) };
    let text = written.as_str().ok_or_else(|| anyhow!("its `path` is {}, no text", written.type_str()))?;
    let path = text.strip_suffix(".node").ok_or_else(|| anyhow!("{text}: no bundle's path (`res://<path>.node`)"))?;
    Ok(Some(std::sync::Arc::from(TreePath::from_res(path)?)))
}

/// The node a field names, where the field is there: as `target_of` reads it.
///
/// # Errors
/// As `target_of`.
pub fn target_in(reference: Option<&Value>) -> Result<Option<Reference>> {
    reference.map_or(Ok(None), target_of)
}

/// What of an asset a reference names, and how it asks for it: what follows its uid (`cells/body?palette=default` of
/// `uid://x/cells/body?palette=default`), or the `output` and `query` of a reference written with its path
/// (`{ uid, path, output = "cells/body", query = { palette = "default" } }`). None where it names the asset itself.
///
/// # Errors
/// What it names is no address, or how it asks is no query.
pub fn asked_of(reference: &Value) -> Result<Option<std::sync::Arc<Asked>>> {
    match reference {
        Value::Table(table) => {
            let Some(output) = table.get("output") else {
                anyhow::ensure!(!table.contains_key("query"), "a reference asks with a query for no output");
                return Ok(None);
            };
            let address = Address::new(output.as_str().ok_or_else(|| anyhow!("its `output` is no text"))?)?;
            let query = match table.get("query") {
                None => String::new(),
                Some(Value::Table(query)) => {
                    let pair = |(key, value): (&String, &Value)| match value {
                        Value::String(value) => Ok(format!("{key}={value}")),
                        other => Err(anyhow!("its query's `{key}` is {}, no text", other.type_str())),
                    };
                    query.iter().map(pair).collect::<Result<Vec<_>>>()?.join("&")
                }
                Some(other) => bail!("its `query` is {}, no table", other.type_str()),
            };
            Ok(Some(Asked::with(address, Query::new(&query)?)))
        }
        Value::String(text) if text.starts_with(UID_PREFIX) => Ok(ResourcePath::new(text)?.asked),
        _ => Ok(None),
    }
}

/// What of an asset a reference names, as it is written: its address, none where it names the asset itself.
///
/// # Errors
/// What it names is no address.
pub fn address_of(reference: &Value) -> Result<Option<&Address>> {
    match reference {
        Value::Table(table) => match table.get("output") {
            Some(output) => Ok(Some(Address::new(output.as_str().ok_or_else(|| anyhow!("its `output` is no text"))?)?)),
            None => Ok(None),
        },
        Value::String(text) => Ok(asked_in(text)?.map(Asked::address)),
        _ => Ok(None),
    }
}

/// A key of how a reference asks for what it names, as it is written: `default` for `palette` of
/// `uid://x/cells/body?palette=default`, or of `{ ..., query = { palette = "default" } }`.
///
/// # Errors
/// How it asks is no query.
pub fn query_of<'a>(reference: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match reference {
        Value::Table(table) => match table.get("query") {
            Some(Value::Table(query)) => match query.get(key) {
                Some(Value::String(value)) => Ok(Some(value)),
                Some(other) => bail!("its query's `{key}` is {}, no text", other.type_str()),
                None => Ok(None),
            },
            Some(other) => bail!("its `query` is {}, no table", other.type_str()),
            None => Ok(None),
        },
        Value::String(text) => Ok(asked_in(text)?.and_then(|asked| asked.query().get(key))),
        _ => Ok(None),
    }
}

/// The part of what a reference names that is read, a step at a time, as it is written: after its `#`, or its
/// `fragment` where it is written with its path. None: all of it.
///
/// # Errors
/// It is no address's form.
pub fn fragment_of(reference: &Value) -> Result<Option<&Address>> {
    let written = match reference {
        Value::Table(table) => match table.get("fragment") {
            Some(fragment) => fragment.as_str().ok_or_else(|| anyhow!("its `fragment` is no text"))?,
            None => return Ok(None),
        },
        Value::String(text) if text.starts_with(UID_PREFIX) => match text.split_once('#') {
            Some((_, fragment)) => fragment,
            None => return Ok(None),
        },
        _ => return Ok(None),
    };
    Ok(Some(Address::new(written)?))
}

/// What a reference written as text names of its asset, and how: what follows its uid.
fn asked_in(text: &str) -> Result<Option<&Asked>> {
    let Some(digits) = text.strip_prefix(UID_PREFIX) else { return Ok(None) };
    let digits = digits.split_once('#').map_or(digits, |(named, _)| named);
    match digits.find(['/', '?']) {
        Some(at) if digits.as_bytes()[at] == b'/' => Ok(Some(Asked::new(&digits[at + 1..])?)),
        Some(_) => bail!("{text} asks with its query for no output"),
        None => Ok(None),
    }
}

/// What a uid's text begins with.
const UID_PREFIX: &str = "uid://";
pub use ui::{
    Align, Canvas, Cell, Color, Marked, Modifiers, Placed, Playing, Role, Shape, Style, Traced, Transport, Ui, UiHost,
    View, ViewportInput,
};

#[cfg(test)]
mod references {
    use super::{Value, address_of, asked_of, fragment_of, query_of, target_of};

    #[test]
    fn a_reference_says_its_asset_what_of_it_how_and_which_part_written_either_way() {
        let text = Value::String("uid://x/cells/body?palette=night#rect/2".to_string());
        let table: Value = "r = { uid = 'uid://x', path = 'res://i.node', output = 'cells/body', query = { palette = 'night' }, fragment = 'rect/2' }"
            .parse::<crate::Table>()
            .unwrap()["r"]
            .clone();
        let x = crate::Reference::new("uid://x").unwrap();
        for reference in [&text, &table] {
            assert_eq!(target_of(reference).unwrap(), Some(x.clone()));
            assert_eq!(asked_of(reference).unwrap().unwrap().to_string(), "cells/body?palette=night");
            assert_eq!(address_of(reference).unwrap().unwrap().as_str(), "cells/body");
            assert_eq!(query_of(reference, "palette").unwrap(), Some("night"));
            assert_eq!(fragment_of(reference).unwrap().unwrap().as_str(), "rect/2");
        }
        let asset = Value::String("uid://x#joints/3".to_string());
        assert_eq!((target_of(&asset).unwrap(), asked_of(&asset).unwrap()), (Some(x.clone()), None));
        assert_eq!(fragment_of(&asset).unwrap().unwrap().as_str(), "joints/3");
        assert!(asked_of(&Value::String("uid://x/cells/body@night".to_string())).is_err());
    }
}
