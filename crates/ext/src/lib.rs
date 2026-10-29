//! The extension SDK: what an extension of the editor is written against.
//!
//! An extension has an id (`pe2.images`), which namespaces everything it registers: its kinds and roles are named
//! `<id>@<name>` (`pe2.images@image`), so no two extensions' names collide and every name says who owns it. It
//! registers procedurally, from one entry point (`extension!`): a kind and what it attaches to it - its fields as the
//! inspector shows them, its picture, its thumbnail, its views - and roles on its own kinds or on its dependencies'.
//!
//! An extension is built as a WebAssembly component (`wasm32-wasip3`): `extension!` then exports it through the
//! contract in `wit/extension.wit` (`guest`), and the host (`project::ext`) loads it with wasmtime. Built for the host's
//! own target, the same crate links into the editor instead and the host calls what it registers directly - the
//! built-ins' fallback while developing. What crosses is what WIT can say: assets lent for a call, images, a `Ui`
//! handle a view draws through, never the host's own types.

mod asset;
mod extract;
#[cfg(target_family = "wasm")]
#[doc(hidden)]
pub mod guest;
mod image;
mod import;
mod project;
mod registry;
mod ui;

pub use anyhow::{Error, Result, anyhow, bail};
pub use asset::{Asset, Source};
pub use extract::{Begin, Declared, Discs, Extraction, ExtractorDef};
pub use image::{Image, Rect, Texture};
pub use import::{Check, Export, FormatDef, Import, Imported, ImporterDef, Made, Recognized, Signature, Sniff, Support};
pub use project::Project;
pub use registry::{
    Extension, Facts, Field, Format, FormatBuilder, Icon, ImporterBuilder, KindBuilder, KindDef, ListParts, MakePanel, MakeView, Name, PanelDef, Part, Parts, Picture,
    Registry, RoleBuilder, RoleDef, Run, Thumbnail, Tint, ToolDef, ViewDef, section,
};
pub use toml::{Table, Value};
pub use ui::{Canvas, Marked, Placed, Style, Ui, UiHost, View};
