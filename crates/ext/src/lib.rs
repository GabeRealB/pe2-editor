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
pub mod data;
pub mod events;
pub mod tokens;
pub mod ty;
mod extract;
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub mod guest;
mod image;
mod import;
pub mod media;
mod project;
pub mod process;
mod registry;
pub mod test;
mod ui;

pub use anyhow::{Error, Result, anyhow, bail};
pub use asset::{Asset, Source};
pub use extract::{Begin, Declared, Discs, Extraction, ExtractorDef, Made};
pub use image::{Image, Mark, Marks, Rect, Texture};
pub use data::{BuildConfig, NodeNamed, Provides, Call, CallHost, Content, File, FileWriter, Generator, Lent, Members, merged, OpDef, OutputDef, Pcm, Port, Retain, RunOp, StartStream, StreamInput, StreamSource, Type, Typed, Value as Data, Writer};
pub use import::{Recognized, Signature, Sniff, TypeDef};
pub use project::Project;
pub use registry::{ActOnContents, CommandDef, HookBuilder, HookDef, ContentAction, ContentDrag, ContentOutcome, ContentPlace, ContentRequest, ContentRow, ContentStyle, Contents, PartField, PartSheet, SheetOfPart, TabDef, TemplateDef, 
    Extension, Field, Format, Icon, MakePanel, MakeView, Name, OpBuilder, PanelDef, Parts,
    Registry, Run, Scope, Setting, Tint, ToolDef, TypeBuilder, ViewDef, section,
};
pub use toml::{Table, Value};

/// What a reference names, as a host call asks for it: a uid or a path, whatever holds it (`{ slot = ... }`), without its output.
#[must_use]
pub fn target_of(reference: &Value) -> Option<&str> {
    let reference = reference.get("slot").unwrap_or(reference);
    let reference = reference.get("slot").unwrap_or(reference);
    match reference {
        Value::String(text) => Some(text.split_once('#').map_or(text.as_str(), |(uid, _)| uid)),
        Value::Table(table) => table.get("uid")?.as_str(),
        _ => None,
    }
}

/// The output of an asset a reference names: what follows its uid (`palettes/default` of
/// `uid://...#palettes/default`), or the `output` of a reference written with its path.
#[must_use]
pub fn output_of(reference: &Value) -> Option<&str> {
    let output = match reference {
        Value::Table(table) => table.get("output")?.as_str()?,
        _ => reference.as_str()?.strip_prefix("uid://")?.split_once('#')?.1,
    };
    (!output.is_empty()).then_some(output)
}
pub use ui::{Canvas, Cell, Marked, Modifiers, Placed, Style, Traced, Ui, UiHost, View, ViewportInput};
